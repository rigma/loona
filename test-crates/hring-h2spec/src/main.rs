#![allow(incomplete_features)]
#![feature(async_fn_in_trait)]

use std::{collections::VecDeque, path::PathBuf};

use hring::{
    http::{StatusCode, Version},
    Body, BodyChunk, Encoder, ExpectResponseHeaders, Headers, Request, Responder, Response,
    ResponseDone, ServerDriver,
};
use tokio::process::Command;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[cfg(feature = "tokio-uring")]
mod uring;

#[cfg(feature = "tokio-uring")]
use uring as server_impl;

#[cfg(not(feature = "tokio-uring"))]
mod non_uring;

#[cfg(not(feature = "tokio-uring"))]
use non_uring as server_impl;

fn main() {
    color_eyre::install().unwrap();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|e| {
            eprintln!("Couldn't parse RUST_LOG: {e}");
            EnvFilter::try_new("info").unwrap()
        }))
        .init();

    let h2spec_binary = match which::which("h2spec") {
        Ok(h2spec_binary) => {
            info!("Using h2spec binary from {}", h2spec_binary.display());
            h2spec_binary
        }
        Err(_) => {
            error!("Couldn't find h2spec binary in PATH, see https://github.com/summerwind/h2spec");
            std::process::exit(1);
        }
    };

    #[cfg(feature = "tokio-uring")]
    hring::tokio_uring::start(async move { real_main(h2spec_binary).await.unwrap() });

    #[cfg(not(feature = "tokio-uring"))]
    {
        use tokio::task::LocalSet;

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let local = LocalSet::new();
                local
                    .run_until(async move { real_main(h2spec_binary).await.unwrap() })
                    .await;
            })
    }
}

struct SDriver;

impl ServerDriver for SDriver {
    async fn handle<E: Encoder>(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<E, ExpectResponseHeaders>,
    ) -> color_eyre::Result<Responder<E, ResponseDone>> {
        tracing::info!(
            "Handling {:?} {}, content_len = {:?}",
            req.method,
            req.uri,
            req_body.content_len()
        );

        let res = Response {
            version: Version::HTTP_2,
            status: StatusCode::OK,
            headers: Headers::default(),
        };
        let mut body = TestBody::default();
        respond.write_final_response_with_body(res, &mut body).await
    }
}

#[derive(Default, Debug)]
struct TestBody {
    eof: bool,
}

impl TestBody {
    const CONTENTS: &str = "I am a test body";
}

impl Body for TestBody {
    fn content_len(&self) -> Option<u64> {
        Some(Self::CONTENTS.len() as _)
    }

    fn eof(&self) -> bool {
        self.eof
    }

    async fn next_chunk(&mut self) -> color_eyre::eyre::Result<BodyChunk> {
        if self.eof {
            Ok(BodyChunk::Done { trailers: None })
        } else {
            self.eof = true;
            Ok(BodyChunk::Chunk(Self::CONTENTS.as_bytes().into()))
        }
    }
}

async fn real_main(h2spec_binary: PathBuf) -> color_eyre::Result<()> {
    let addr = server_impl::spawn_server("[::]:0".parse()?).await?;

    let mut args = std::env::args().skip(1).collect::<VecDeque<_>>();
    if matches!(args.get(0).map(|s| s.as_str()), Some("--")) {
        args.pop_front();
    }
    tracing::info!("Custom args: {args:?}");

    Command::new(h2spec_binary)
        .arg("-p")
        .arg(&format!("{}", addr.port()))
        .arg("-o")
        .arg("1")
        .args(&args)
        .spawn()?
        .wait()
        .await?;

    Ok(())
}
