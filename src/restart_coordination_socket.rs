//! Communication with a running process over a unix domain socket.
use crate::RestartResult;
use anyhow::{anyhow, Context};
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tokio_util::codec::{Decoder, Framed};

/// Represents the restart coordination socket, used for communicating with a running oxy process.
/// This is used to trigger a restart and receive notification of its completion or failure.
pub struct RestartCoordinationSocket {
    codec: Framed<UnixStream, LengthDelimitedCodec>,
}

impl RestartCoordinationSocket {
    /// Create a new RestartCoordinationSocket wrapping a unix socket.
    pub fn new(socket: UnixStream) -> Self {
        RestartCoordinationSocket {
            codec: LengthDelimitedCodec::new().framed(socket),
        }
    }

    /// Sends a restart command through the socket. Returns Ok(child_pid) on success or an error
    /// if the restart failed for any reason.
    pub async fn send_restart_command(&mut self) -> RestartResult<u32> {
        self.send_message(RestartMessage::Request(RestartRequest::TryRestart))
            .await?;
        match self.receive_message().await? {
            RestartMessage::Response(RestartResponse::RestartComplete(pid)) => Ok(pid),
            RestartMessage::Response(RestartResponse::RestartFailed(reason)) => {
                Err(anyhow!(reason))
            }
            _ => Err(anyhow!("unexpected message received")),
        }
    }

    /// Send a message over the socket
    pub async fn send_message(&mut self, msg: RestartMessage) -> RestartResult<()> {
        self.codec
            .send(serde_json::to_string(&msg).unwrap().into())
            .await?;

        Ok(())
    }

    /// Receive a message from the socket.
    pub async fn receive_message(&mut self) -> RestartResult<RestartMessage> {
        let message = self
            .codec
            .next()
            .await
            .context("connection closed while awaiting a message")??;

        Ok(serde_json::from_slice(&message)?)
    }
}

/// Represents any message that may be sent over the socket.
#[derive(Debug, Serialize, Deserialize)]
pub enum RestartMessage {
    Request(RestartRequest),
    Response(RestartResponse),
}

/// A request message that expects a response.
#[derive(Debug, Serialize, Deserialize)]
pub enum RestartRequest {
    TryRestart,
}

/// A response to a request message.
#[derive(Debug, Serialize, Deserialize)]
pub enum RestartResponse {
    // Restart completed. The child PID is provided.
    RestartComplete(u32),
    // Restart failed. The error message is attached.
    RestartFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_restart_complete() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut client = RestartCoordinationSocket::new(client);
        let mut server = RestartCoordinationSocket::new(server);
        let child_pid = 42;

        tokio::spawn(async move {
            let message = server.receive_message().await.unwrap();
            assert!(matches!(
                message,
                RestartMessage::Request(RestartRequest::TryRestart)
            ));
            let response = RestartMessage::Response(RestartResponse::RestartComplete(child_pid));
            server.send_message(response).await.unwrap();
        });

        assert_eq!(client.send_restart_command().await.unwrap(), child_pid);
    }

    #[tokio::test]
    async fn test_restart_failed() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut client = RestartCoordinationSocket::new(client);
        let mut server = RestartCoordinationSocket::new(server);
        let error_message = "huge success";

        tokio::spawn(async move {
            let message = server.receive_message().await.unwrap();
            assert!(matches!(
                message,
                RestartMessage::Request(RestartRequest::TryRestart)
            ));
            let response =
                RestartMessage::Response(RestartResponse::RestartFailed(error_message.into()));
            server.send_message(response).await.unwrap();
        });

        let r = client.send_restart_command().await;
        assert_eq!(r.err().map(|e| e.to_string()), Some(error_message.into()));
    }
}
