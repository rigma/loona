use std::fmt;

use buffet::Piece;
use http::{header, StatusCode};

use crate::{Body, BodyChunk, Headers, HeadersExt, Response};

pub trait ResponseState {}

pub struct ExpectResponseHeaders;
impl ResponseState for ExpectResponseHeaders {}

pub struct ExpectResponseBody {
    pub announced_content_length: Option<u64>,
    pub bytes_written: u64,
}
impl ResponseState for ExpectResponseBody {}

pub struct ResponseDone;
impl ResponseState for ResponseDone {}

#[derive(Debug)]
#[non_exhaustive]
pub enum ResponderError<E>
where
    E: std::error::Error,
{
    /// The response status code was not 1xx
    InterimResponseMustHaveStatusCode1xx { actual: StatusCode },

    /// The response status code was not >= 200
    FinalResponseMustHaveStatusCodeGreaterThanOrEqualTo200 { actual: StatusCode },

    /// Got an encoder error
    EncoderError(E),
}

impl<E> From<E> for ResponderError<E>
where
    E: std::error::Error,
{
    fn from(e: E) -> Self {
        Self::EncoderError(e)
    }
}

impl fmt::Display for ResponderError<http::Error> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InterimResponseMustHaveStatusCode1xx { actual } => {
                write!(
                    f,
                    "interim response must have status code 1xx, got {actual}"
                )
            }
            Self::FinalResponseMustHaveStatusCodeGreaterThanOrEqualTo200 { actual } => {
                write!(
                    f,
                    "final response must have status code >= 200, got {actual}"
                )
            }
            Self::EncoderError(e) => write!(f, "encoder error: {e}"),
        }
    }
}

impl<E> std::error::Error for ResponderError<E> where E: std::error::Error {}

pub struct Responder<E, S>
where
    E: Encoder,
    S: ResponseState,
{
    encoder: E,
    state: S,
}

impl<E> Responder<E, ExpectResponseHeaders>
where
    E: Encoder,
{
    pub fn new(encoder: E) -> Self {
        Self {
            encoder,
            state: ExpectResponseHeaders,
        }
    }

    /// Send an informational status code, cf. <https://httpwg.org/specs/rfc9110.html#status.1xx>
    /// Errors out if the response status is not 1xx
    pub async fn write_interim_response(&mut self, res: Response) -> Result<(), ResponderError> {
        if !res.status.is_informational() {
            return Err(ResponderError::InterimResponseMustHaveStatusCode1xx {
                actual: res.status,
            });
        }

        self.encoder.write_response(res).await?;
        Ok(())
    }

    async fn write_final_response_internal(
        mut self,
        res: Response,
        announced_content_length: Option<u64>,
    ) -> Result<Responder<E, ExpectResponseBody>, ResponderError> {
        if res.status.is_informational() {
            return Err(
                ResponderError::FinalResponseMustHaveStatusCodeGreaterThanOrEqualTo200 {
                    actual: res.status,
                },
            );
        }
        self.encoder.write_response(res).await?;
        Ok(Responder {
            state: ExpectResponseBody {
                announced_content_length,
                bytes_written: 0,
            },
            encoder: self.encoder,
        })
    }

    /// Send the final response headers
    /// Errors out if the response status is < 200.
    /// Errors out if the client sent `expect: 100-continue`
    pub async fn write_final_response(
        self,
        res: Response,
    ) -> ResponderResult<Responder<E, ExpectResponseBody>> {
        let announced_content_length = res.headers.content_length();
        self.write_final_response_internal(res, announced_content_length)
            .await
    }

    /// Writes a response with the given body. Sets `content-length` or
    /// `transfer-encoding` as needed.
    pub async fn write_final_response_with_body(
        self,
        mut res: Response,
        body: &mut impl Body,
    ) -> ResponderResult<Responder<E, ResponseDone>> {
        if let Some(clen) = body.content_len() {
            res.headers
                .entry(header::CONTENT_LENGTH)
                .or_insert_with(|| {
                    // TODO: can probably get rid of this heap allocation, also
                    // use `itoa`, cf. https://docs.rs/itoa/latest/itoa/
                    // TODO: also, for `write_final_response`, we could avoid
                    // parsing the content-length header, since we have the
                    // content-length already.
                    // TODO: also, instead of doing a heap allocation with a `Vec<u8>`, we
                    // could probably take a tiny bit of `RollMut` which is cheaper to
                    // allocate (or at least was, last I measured).
                    format!("{clen}").into_bytes().into()
                });
        }

        let mut this = self.write_final_response(res).await?;

        loop {
            match body.next_chunk().await? {
                BodyChunk::Chunk(chunk) => {
                    this.write_chunk(chunk).await?;
                }
                BodyChunk::Done { trailers } => {
                    return this.finish_body(trailers).await;
                }
            }
        }
    }
}

impl<E> Responder<E, ExpectResponseBody>
where
    E: Encoder,
{
    /// Send a response body chunk. Errors out if sending more than the
    /// announced content-length.
    #[inline]
    pub async fn write_chunk(&mut self, chunk: Piece) -> ResponderResult<()> {
        self.state.bytes_written += chunk.len() as u64;
        self.encoder.write_body_chunk(chunk).await
    }

    /// Finish the body, with optional trailers, cf. <https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/TE>
    /// Errors out if the sent body doesn't match the announced content-length.
    /// Errors out if trailers that weren't announced are being sent, or if the
    /// client didn't explicitly announce it accepted trailers, or if the
    /// response is a 204, 205 or 304, or if the body wasn't sent with
    /// chunked transfer encoding.
    pub async fn finish_body(
        mut self,
        trailers: Option<Box<Headers>>,
    ) -> ResponderResult<Responder<E, ResponseDone>> {
        if let Some(announced_content_length) = self.state.announced_content_length {
            if self.state.bytes_written != announced_content_length {
                eyre::bail!(
                    "content-length mismatch: announced {announced_content_length}, wrote {}",
                    self.state.bytes_written
                );
            }
        }
        self.encoder.write_body_end().await?;

        if let Some(trailers) = trailers {
            self.encoder.write_trailers(trailers).await?;
        }

        Ok(Responder {
            state: ResponseDone,
            encoder: self.encoder,
        })
    }
}

impl<E> Responder<E, ResponseDone>
where
    E: Encoder,
{
    pub fn into_inner(self) -> E {
        self.encoder
    }
}

pub type ResponderResult<T> = Result<T, ResponderError>;

#[allow(async_fn_in_trait)] // we never require Send
pub trait Encoder {
    type Error: std::error::Error;

    async fn write_response(&mut self, res: Response) -> Result<(), Self::Error>;
    async fn write_body_chunk(&mut self, chunk: Piece) -> Result<(), Self::Error>;
    async fn write_body_end(&mut self) -> Result<(), Self::Error>;
    async fn write_trailers(&mut self, trailers: Box<Headers>) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use buffet::Piece;
    use http::{StatusCode, Version};

    #[derive(Clone, Copy)]
    struct MockEncoder;

    impl Encoder for MockEncoder {
        async fn write_response(&mut self, _: Response) -> ResponderResult<()> {
            Ok(())
        }
        async fn write_body_chunk(&mut self, _: Piece) -> ResponderResult<()> {
            Ok(())
        }
        async fn write_body_end(&mut self) -> ResponderResult<()> {
            Ok(())
        }
        async fn write_trailers(&mut self, _: Box<Headers>) -> ResponderResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_content_length_mismatch() {
        let encoder = MockEncoder;
        for version in [Version::HTTP_11, Version::HTTP_2] {
            let mut res = Response {
                status: StatusCode::OK,
                version,
                headers: Default::default(),
            };
            res.headers.insert(header::CONTENT_LENGTH, "10".into());

            // Test writing fewer bytes than announced
            let mut responder = Responder::new(encoder)
                .write_final_response(res.clone())
                .await
                .unwrap();
            responder.write_chunk(b"12345".into()).await.unwrap();
            let result = responder.finish_body(None).await;
            assert!(result.is_err());
            assert!(matches!(result, Err(e) if e.to_string().contains("content-length mismatch")));

            // Test writing more bytes than announced
            let encoder = MockEncoder;
            let mut responder = Responder::new(encoder)
                .write_final_response(res)
                .await
                .unwrap();
            responder.write_chunk(b"12345678901".into()).await.unwrap();
            let result = responder.finish_body(None).await;
            assert!(result.is_err());
            assert!(matches!(result, Err(e) if e.to_string().contains("content-length mismatch")));
        }
    }
}
