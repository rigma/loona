use std::rc::Rc;

use fluke_buffet::{IntoHalves, Piece, WriteOwned};
use fluke_h2_parse::Frame;

pub mod rfc9113;

pub struct TestGroup<IO> {
    pub name: String,
    pub tests: Vec<Box<dyn Test<IO>>>,
}

pub struct Conn<IO: IntoHalves + 'static> {
    r: <IO as IntoHalves>::Read,
    w: <IO as IntoHalves>::Write,
}

impl<IO: IntoHalves> Conn<IO> {
    pub fn new(io: IO) -> Self {
        let (r, w) = io.into_halves();
        Self { r, w }
    }

    pub async fn send(&mut self, buf: impl Into<Piece>) -> eyre::Result<()> {
        self.w.write_all_owned(buf.into()).await?;
        Ok(())
    }

    pub async fn write_frame(&mut self, frame: Frame) -> eyre::Result<()> {
        let mut buf = Vec::new();
        frame.write_into(&mut buf)?;
        self.send(buf).await
    }
}

pub struct Config {}

pub trait Test<IO: IntoHalves + 'static> {
    fn name(&self) -> &'static str;
    fn run(
        &self,
        config: Rc<Config>,
        conn: Conn<IO>,
    ) -> futures_util::future::LocalBoxFuture<eyre::Result<()>>;
}

pub fn all_groups<IO: IntoHalves + 'static>() -> Vec<TestGroup<IO>> {
    vec![rfc9113::group()]
}

#[macro_export]
macro_rules! test_struct {
    ($name: expr, $fn: ident, $struct: ident) => {
        #[derive(Default)]
        pub struct $struct {}

        impl<IO: IntoHalves + 'static> Test<IO> for $struct {
            fn name(&self) -> &'static str {
                $name
            }

            fn run(
                &self,
                config: std::rc::Rc<Config>,
                conn: Conn<IO>,
            ) -> futures_util::future::LocalBoxFuture<eyre::Result<()>> {
                Box::pin($fn(config, conn))
            }
        }
    };
}

#[macro_export]
macro_rules! gen_tests {
    ($body: tt) => {
        #[cfg(test)]
        mod rfc9113 {
            use ::httpwg::rfc9113 as __rfc;

            #[test]
            fn test_3_4() {
                use __rfc::Test3_4 as Test;
                $body
            }

            #[test]
            fn test_4_1() {
                use __rfc::Test4_1 as Test;
                $body
            }
        }
    };
}
