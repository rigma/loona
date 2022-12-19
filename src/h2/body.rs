use tokio::sync::mpsc;

use crate::{Body, BodyChunk, Roll};

#[derive(Debug)]
pub(crate) struct H2Body {
    pub(crate) content_length: Option<u64>,
    pub(crate) eof: bool,
    // TODO: more specific error handling
    pub(crate) rx: mpsc::Receiver<eyre::Result<Roll>>,
}

impl Body for H2Body {
    fn content_len(&self) -> Option<u64> {
        self.content_length
    }

    fn eof(&self) -> bool {
        self.eof
    }

    async fn next_chunk(&mut self) -> eyre::Result<BodyChunk> {
        let chunk = if self.eof {
            BodyChunk::Done { trailers: None }
        } else {
            match self.rx.recv().await {
                Some(roll) => BodyChunk::Chunk(roll?.into()),
                // TODO: handle trailers
                None => {
                    self.eof = true;
                    BodyChunk::Done { trailers: None }
                }
            }
        };
        Ok(chunk)
    }
}
