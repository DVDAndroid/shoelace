#[macro_use]
extern crate lazy_static;

mod api;
mod config;
mod error;
mod front;
mod proxy;
mod req;

use actix_web::{
    middleware::{Compat, Logger},
    web, App, HttpServer,
};
use actix_web_static_files::ResourceFiles;
use config::{ProxyModes, Settings};
use include_dir::{include_dir, Dir};
use std::{collections::HashMap, fs::File, io::BufReader};
use tera::Tera;
use tokio::sync::Mutex;
use tracing_actix_web::TracingLogger;

#[allow(unused)]
pub(crate) struct ShoelaceData {
    pub(crate) keystore_type: config::ProxyModes,
    pub(crate) internal_store: Option<Mutex<HashMap<String, String>>>,
    pub(crate) redis: Option<redis::aio::MultiplexedConnection>,
    pub(crate) rocksdb: Option<rocksdb::DB>,
    pub(crate) base_url: String,
}

// Bundle in folders on compile time
pub static TEMPLATES_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/templates");
include!(concat!(env!("OUT_DIR"), "/generated.rs"));

// Import templates
lazy_static! {
    pub static ref TEMPLATES: Tera = {
        let mut tera = Tera::default();

        let templates: Vec<(&str, &str)> = TEMPLATES_DIR
            .find("**/*.html")
            .expect("Templates not found")
            .map(|file| {
                (
                    file.path().to_str().unwrap_or(""),
                    file.as_file()
                        .expect("Not a file")
                        .contents_utf8()
                        .unwrap_or(""),
                )
            })
            .collect::<Vec<(&str, &str)>>();

        match tera.add_raw_templates(templates) {
            Ok(_) => tera,
            Err(error) => {
                println!("Parsing error(s): {}", error);
                ::std::process::exit(1)
            }
        }
    };
}

/// Web server
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Initialize logger
    tracing_subscriber::fmt::init();

    // Parses config
    let maybe_config = config::Settings::new();
    let config: Settings;

    if let Ok(got_config) = maybe_config {
        config = got_config
    } else {
        return Err(maybe_config
            .map_err(|x| std::io::Error::new(std::io::ErrorKind::InvalidInput, x))
            .unwrap_err());
    }

    // Defines application data
    let data = web::Data::new(ShoelaceData {
        keystore_type: config.proxy.backend.to_owned(),
        rocksdb: match &config.proxy.backend {
            ProxyModes::RocksDB => Some(
                rocksdb::DB::open_default(config.proxy.rocksdb.unwrap().path)
                    .expect("couldn't open database"),
            ),
            _ => None,
        },
        redis: match &config.proxy.backend {
            ProxyModes::Redis => Some({
                let client = redis::Client::open(config.proxy.redis.unwrap().uri).unwrap();

                client
                    .get_multiplexed_async_connection()
                    .await
                    .expect("couldn't connect to redis")
            }),
            _ => None,
        },
        internal_store: match &config.proxy.backend {
            ProxyModes::Internal => Some(Mutex::new(HashMap::new())),
            _ => None,
        },
        base_url: config.server.base_url,
    });

    // Start up web server
    let mut server = HttpServer::new(move || {
        let mut app = App::new()
            .wrap(Compat::new(TracingLogger::default()))
            .wrap(Logger::default());

        if config.endpoint.frontend {
            let generated = generate();

            app = app
                .service(ResourceFiles::new("/static", generated))
                .service(front::user)
                .service(front::post)
                .service(front::home)
                .service(front::find)
                .service(front::redirect)
        }

        if config.endpoint.api {
            app = app.service(web::scope("/api/v1").service(api::post).service(api::user))
        }

        if !matches!(config.proxy.backend, ProxyModes::None) {
            app = app.service(web::scope("/proxy").service(proxy::proxy));
        }

        app = app
            .default_service(web::to(move || error::not_found(config.endpoint.frontend)))
            .app_data(data.clone());

        app
    });

    if config.server.tls.enabled {
        let mut certs_file = BufReader::new(
            File::open(config.server.tls.cert).expect("Unable to open certficate file"),
        );
        let mut key_file = BufReader::new(
            File::open(config.server.tls.key).expect("Unable to open certficate file"),
        );

        let tls_certs = rustls_pemfile::certs(&mut certs_file)
            .collect::<Result<Vec<_>, _>>()
            .expect("Not a certificate chain");

        let tls_key = rustls_pemfile::pkcs8_private_keys(&mut key_file)
            .next()
            .expect("Not a key file")
            .expect("Not a key file");

        let tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(tls_certs, rustls::pki_types::PrivateKeyDer::Pkcs8(tls_key))
            .expect("Unable to configure TLS");

        server = server.bind_rustls_0_22((config.server.listen, config.server.port), tls_config)?;
    } else {
        server = server.bind((config.server.listen, config.server.port))?;
    }

    server.run().await
}
