#[macro_use]
extern crate lazy_static;

mod api;
mod config;
mod error;
mod front;
mod proxy;
mod req;

use actix_files::Files;
use actix_web::{
    middleware::{Compat, Logger},
    web, App, HttpServer,
};
use config::{ProxyModes, Redis, Settings};
use std::collections::HashMap;
use tera::Tera;
use tokio::sync::Mutex;
use tracing_actix_web::TracingLogger;

#[allow(unused)]
pub(crate) struct ShoelaceData {
    pub(crate) keystore_type: config::ProxyModes,
    pub(crate) internal_store: Option<Mutex<HashMap<String, String>>>,
    pub(crate) redis: Option<Redis>,
    pub(crate) rocksdb: Option<rocksdb::DB>,
    pub(crate) base_url: String,
}

// Import templates
lazy_static! {
    pub static ref TEMPLATES: Tera = {
        match Tera::new(concat!(env!("CARGO_MANIFEST_DIR"), "/templates/**/*")) {
            Ok(t) => t,
            Err(e) => {
                println!("Parsing error(s): {}", e);
                ::std::process::exit(1);
            }
        }
    };
}

/// Web server
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Initialize logger
    tracing_subscriber::fmt::init();

    let maybe_config = config::Settings::new();
    let config: Settings;

    if let Ok(got_config) = maybe_config {
        config = got_config
    } else {
        return Err(maybe_config
            .map_err(|x| std::io::Error::new(std::io::ErrorKind::InvalidInput, x))
            .unwrap_err());
    }

    let data = web::Data::new(ShoelaceData {
        keystore_type: config.proxy.mode.to_owned(),
        rocksdb: match &config.proxy.mode {
		ProxyModes::RocksDB => Some(rocksdb::DB::open_default(config.proxy.rocksdb.unwrap().path).expect("couldn't open database")),
		_ => None,
	},
        redis: config.proxy.redis,
        internal_store: match &config.proxy.mode {
		ProxyModes::Internal => Some(Mutex::new(HashMap::new())),
		_ => None,
	},
        base_url: config.server.base_url,
    });

    // Start up web server
    HttpServer::new(move || {
        let mut app = App::new()
            .wrap(Compat::new(TracingLogger::default()))
            .wrap(Logger::default());

        if config.endpoint.frontend {
            app = app
                .service(Files::new(
                    "/static",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/static"),
                ))
                .service(front::user)
                .service(front::post)
                .service(front::home)
                .service(front::find)
                .service(front::redirect)
        }

        if config.endpoint.api {
            app = app.service(web::scope("/api/v1").service(api::post).service(api::user))
        }

        app = app
            .service(web::scope("/proxy").service(proxy::proxy))
            .default_service(web::to(move || error::not_found(config.endpoint.frontend)))
            .app_data(data.clone());

        app
    })
    .bind((config.server.listen, config.server.port))?
    .run()
    .await
}
