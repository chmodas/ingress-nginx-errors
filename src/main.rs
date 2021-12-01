use std::fs::OpenOptions;
use std::io::{BufReader, Read};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::exit;
use std::str::FromStr;
use std::task::{Context, Poll};

use clap::{App, Arg, ArgMatches};
use futures_util::future;
use hyper::{Body, Request, Response, Server};
use hyper::service::Service;
use mime::Mime;

const ROOT: &str = "/";

/// The name of the header used as source of the HTTP status code to return
const CODE_HEADER: &str = "X-Code";

/// The name of the header used to extract the return format, which is the value of
/// the Accept header sent by the client.
const FORMAT_HEADER: &str = "X-Format";

/// The format that will be used by default if the FORMAT_HEADER is not specified
const DEFAULT_FORMAT: &str = "html";

const DEFAULT_CODE: u32 = 404;

#[derive(Debug)]
pub struct Svc {
    templates_dir: PathBuf,
}

impl Svc {
    pub fn new(templates_dir: PathBuf) -> Self {
        Self { templates_dir }
    }
}

impl Service<Request<Body>> for Svc {
    type Response = Response<Body>;
    type Error = hyper::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let rsp = Response::builder();

        let uri = req.uri();
        if uri.path() != ROOT {
            let body = Body::from(Vec::new());
            let rsp = rsp.status(404).body(body).unwrap();
            return future::ok(rsp);
        }

        // Assign the default code if the X-Code header is missing or
        // it doesn't contain an unsigned integer.
        let code = req.headers().get(CODE_HEADER)
            .map(|value| value.to_str()
                .map(|x| match x.parse::<u32>() {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("Unexpected error reading return code: {}. Using {}", error, DEFAULT_CODE);
                        DEFAULT_CODE
                    }
                })
                .unwrap_or(DEFAULT_CODE))
            .unwrap_or(DEFAULT_CODE);

        let response = req.headers().get(FORMAT_HEADER)
            .map(|value| match value.to_str() {
                Ok(ct) => {
                    match Mime::from_str(ct) {
                        Ok(mime) => {
                            format!("{}.{}", code, mime.subtype())
                        }
                        Err(error) => {
                            eprintln!("Unexpected error reading the media type: {}. Using {}", error, DEFAULT_FORMAT);
                            format!("{}.{}", code, DEFAULT_FORMAT)
                        }
                    }
                }
                Err(_) => {
                    format!("{}.{}", code, DEFAULT_FORMAT)
                }
            })
            .unwrap_or(format!("{}.{}", code, DEFAULT_FORMAT));

        self.templates_dir.push(response);
        return future::ok(match OpenOptions::new().read(true).open(&self.templates_dir) {
            Ok(file) => {
                let mut reader = BufReader::new(file);
                let mut buffer = Vec::new();
                reader.read_to_end(&mut buffer).unwrap();

                let body = Body::from(buffer);
                rsp.status(200).body(body).unwrap()
            }
            Err(error) => {
                eprintln!("Unexpected error reading the template file {:?}: {}", &self.templates_dir, error);
                let body = Body::from(Vec::new());
                rsp.status(404).body(body).unwrap()
            }
        });
    }
}

pub struct MakeSvc {
    templates_dir: PathBuf,
}

impl MakeSvc {
    pub fn new(templates_dir: PathBuf) -> Self {
        Self { templates_dir }
    }
}

impl<T> Service<T> for MakeSvc {
    type Response = Svc;
    type Error = std::io::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, _: T) -> Self::Future {
        future::ok(Svc::new(self.templates_dir.clone()))
    }
}


/// Parse the CLI arguments.
fn command_line_interface<'a>() -> ArgMatches<'a> {
    App::new("ingress-nginx-errors")
        .version("0.1")
        .arg(
            Arg::with_name("listen-address")
                .help("Address to listen on for API and telemetry.")
                .short("l")
                .long("listen-address")
                .default_value("0.0.0.0:3000")
                .value_name("listen-address")
                .takes_value(true)
        )
        .arg(
            Arg::with_name("templates-dir")
                .help("The path to the directory containing the template files.")
                .short("p")
                .long("templates-dir")
                .value_name("templates-dir")
                .takes_value(true)
        )
        .get_matches()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = command_line_interface();
    let addr = matches
        .value_of("listen-address")
        .expect("The listen-address must be provided")
        .parse::<SocketAddr>()?;
    let templates_dir = matches
        .value_of("templates-dir")
        .map(PathBuf::from)
        .expect("The path to the response files must be provided");

    if !templates_dir.exists() {
        eprintln!("The templates path {:?} does not exist", templates_dir);
        exit(1)
    } else if !templates_dir.is_dir() {
        eprintln!("The templates path {:?} is not a directory", templates_dir);
        exit(1)
    }

    let server = Server::bind(&addr).serve(MakeSvc::new(templates_dir));

    println!("Listening on http://{}", addr);

    server.await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use hyper::body;
    use hyper::body::Bytes;

    use super::*;

    /// Constructs a GET request to the given URI with the specified headers
    fn req(uri: &str, headers: Vec<(&str, &str)>) -> Request<Body> {
        let mut request = Request::get(uri);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        request.body(Body::from(Vec::new())).unwrap()
    }

    #[tokio::test]
    async fn formatted_as_json() -> Result<(), Box<dyn Error>> {
        let errs = vec![
            ("404", "The page you're looking for could not be found"),
            ("500", "Internal server error"),
        ];
        for (code, message) in errs {
            let request = req("/", vec![("X-Code", code), ("X-Format", "application/json")]);

            let mut response = Svc::new(PathBuf::from("./files")).call(request).await?;
            assert_eq!(response.status(), 200);

            let body = body::to_bytes(response.body_mut()).await?;
            assert_eq!(body, Bytes::from(format!(r#"{{"message":"{}"}}"#, message)));
        }

        Ok(())
    }

    #[tokio::test]
    async fn formatted_as_html() -> Result<(), Box<dyn Error>> {
        let errs = vec![
            ("404", "The page you're looking for could not be found"),
            ("500", "Internal server error"),
        ];
        for (code, message) in errs {
            let request = req("/", vec![("X-Code", code), ("X-Format", "text/html")]);

            let mut response = Svc::new(PathBuf::from("./files")).call(request).await?;
            assert_eq!(response.status(), 200);

            let body = body::to_bytes(response.body_mut()).await?;
            assert_eq!(body, Bytes::from(format!("<span>{}</span>", message)));
        }

        Ok(())
    }

    #[tokio::test]
    async fn html_by_default() -> Result<(), Box<dyn Error>> {
        let request = req("/", vec![("X-Code", "500")]);

        let mut response = Svc::new(PathBuf::from("files")).call(request).await?;
        assert_eq!(response.status(), 200);

        let body = body::to_bytes(response.body_mut()).await?;
        assert_eq!(body, Bytes::from("<span>Internal server error</span>"));

        Ok(())
    }

    #[tokio::test]
    async fn picks_404_for_erroneous_code() -> Result<(), Box<dyn Error>> {
        let request = req("/", vec![("X-Code", "x500")]);

        let mut response = Svc::new(PathBuf::from("files")).call(request).await?;
        assert_eq!(response.status(), 200);

        let body = body::to_bytes(response.body_mut()).await?;
        assert_eq!(body, Bytes::from("<span>The page you're looking for could not be found</span>"));

        Ok(())
    }

    #[tokio::test]
    async fn empty_404_for_codes_without_files() -> Result<(), Box<dyn Error>> {
        let request = req("/", vec![("X-Code", "403")]);

        let mut response = Svc::new(PathBuf::from("files")).call(request).await?;
        assert_eq!(response.status(), 404);

        let body = body::to_bytes(response.body_mut()).await?;
        assert!(body.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn empty_404_for_requests_to_pages_other_than_root() -> Result<(), Box<dyn Error>> {
        let request = req("/boo", vec![("X-Code", "403")]);

        let mut response = Svc::new(PathBuf::from("files")).call(request).await?;
        assert_eq!(response.status(), 404);

        let body = body::to_bytes(response.body_mut()).await?;
        assert!(body.is_empty());

        Ok(())
    }
}
