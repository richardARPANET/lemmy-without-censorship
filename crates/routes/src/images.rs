use actix_http::http::header::CONTENT_ENCODING;
use actix_web::{body::BodyStream, http::StatusCode, web::Data, *};
use anyhow::anyhow;
use awc::Client;
use lemmy_utils::{claims::Claims, rate_limit::RateLimit, settings::structs::Settings, LemmyError};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub fn config(cfg: &mut web::ServiceConfig, rate_limit: &RateLimit) {
  let client = Client::builder()
    .header("User-Agent", "pict-rs-frontend, v0.1.0")
    .timeout(Duration::from_secs(30))
    .finish();

  cfg
    .app_data(Data::new(client))
    .service(
      web::resource("/pictrs/image")
        .wrap(rate_limit.image())
        .route(web::post().to(upload)),
    )
    // This has optional query params: /image/{filename}?format=jpg&thumbnail=256
    .service(web::resource("/pictrs/image/{filename}").route(web::get().to(full_res)))
    .service(web::resource("/pictrs/image/delete/{token}/{filename}").route(web::get().to(delete)));
}

#[derive(Debug, Serialize, Deserialize)]
struct Image {
  file: String,
  delete_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Images {
  msg: String,
  files: Option<Vec<Image>>,
}

#[derive(Deserialize)]
struct PictrsParams {
  format: Option<String>,
  thumbnail: Option<String>,
}

async fn upload(
  req: HttpRequest,
  body: web::Payload,
  client: web::Data<Client>,
) -> Result<HttpResponse, Error> {
  // TODO: check rate limit here
  let jwt = req
    .cookie("jwt")
    .expect("No auth header for picture upload");

  if Claims::decode(jwt.value()).is_err() {
    return Ok(HttpResponse::Unauthorized().finish());
  };

  let mut client_req = client.request_from(format!("{}/image", pictrs_url()?), req.head());
  // remove content-encoding header so that pictrs doesnt send gzipped response
  client_req.headers_mut().remove(CONTENT_ENCODING);

  if let Some(addr) = req.head().peer_addr {
    client_req = client_req.insert_header(("X-Forwarded-For", addr.to_string()))
  };

  let mut res = client_req
    .send_stream(body)
    .await
    .map_err(error::ErrorBadRequest)?;

  let images = res.json::<Images>().await.map_err(error::ErrorBadRequest)?;

  Ok(HttpResponse::build(res.status()).json(images))
}

async fn full_res(
  filename: web::Path<String>,
  web::Query(params): web::Query<PictrsParams>,
  req: HttpRequest,
  client: web::Data<Client>,
) -> Result<HttpResponse, Error> {
  let name = &filename.into_inner();

  // If there are no query params, the URL is original
  let url = if params.format.is_none() && params.thumbnail.is_none() {
    format!("{}/image/original/{}", pictrs_url()?, name,)
  } else {
    // Use jpg as a default when none is given
    let format = params.format.unwrap_or_else(|| "jpg".to_string());

    let mut url = format!("{}/image/process.{}?src={}", pictrs_url()?, format, name,);

    if let Some(size) = params.thumbnail {
      url = format!("{}&thumbnail={}", url, size,);
    }
    url
  };

  image(url, req, client).await
}

async fn image(
  url: String,
  req: HttpRequest,
  client: web::Data<Client>,
) -> Result<HttpResponse, Error> {
  let mut client_req = client.request_from(url, req.head());
  client_req.headers_mut().remove(CONTENT_ENCODING);

  if let Some(addr) = req.head().peer_addr {
    client_req = client_req.insert_header(("X-Forwarded-For", addr.to_string()))
  };

  let res = client_req
    .no_decompress()
    .send()
    .await
    .map_err(error::ErrorBadRequest)?;

  if res.status() == StatusCode::NOT_FOUND {
    return Ok(HttpResponse::NotFound().finish());
  }

  let mut client_res = HttpResponse::build(res.status());

  for (name, value) in res.headers().iter().filter(|(h, _)| *h != "connection") {
    client_res.insert_header((name.clone(), value.clone()));
  }

  Ok(client_res.body(BodyStream::new(res)))
}

async fn delete(
  components: web::Path<(String, String)>,
  req: HttpRequest,
  client: web::Data<Client>,
) -> Result<HttpResponse, Error> {
  let (token, file) = components.into_inner();

  let url = format!("{}/image/delete/{}/{}", pictrs_url()?, &token, &file);

  let mut client_req = client.request_from(url, req.head());
  client_req.headers_mut().remove(CONTENT_ENCODING);

  if let Some(addr) = req.head().peer_addr {
    client_req = client_req.insert_header(("X-Forwarded-For", addr.to_string()))
  };

  let res = client_req
    .no_decompress()
    .send()
    .await
    .map_err(error::ErrorBadRequest)?;

  Ok(HttpResponse::build(res.status()).body(BodyStream::new(res)))
}

fn pictrs_url() -> Result<String, LemmyError> {
  Settings::get()
    .pictrs_url
    .ok_or_else(|| anyhow!("images_disabled").into())
}
