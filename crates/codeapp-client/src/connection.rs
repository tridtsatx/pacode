//! `Client`: one connection with reconnect.

use codeapp_types::{Attach, Reply, Request, SessionId, SessionSnapshot};

use crate::{ClientError, ClientOptions, EventReceiver};

pub struct Client {
    _private: (),
}

impl Client {
    /// Connect (spawning the daemon when allowed), send `Hello`, start the reader.
    /// The returned receiver yields `ClientEvent`s (the first is `Connected`).
    pub async fn connect(opts: ClientOptions) -> Result<(Client, EventReceiver), ClientError> {
        let _ = opts;
        todo!("Client::connect")
    }

    /// Attach to a session and return its snapshot; remembered for reconnects.
    pub async fn attach(&self, attach: Attach) -> Result<SessionSnapshot, ClientError> {
        let _ = attach;
        todo!("Client::attach")
    }

    pub fn session(&self) -> Option<SessionId> {
        todo!("Client::session")
    }

    /// Send a request and await its reply (with `request_timeout`).
    pub async fn request(&self, req: Request) -> Result<Reply, ClientError> {
        let _ = req;
        todo!("Client::request")
    }

    /// `request` expecting `Reply::Ok`; `Reply::Error` becomes `ClientError::Daemon`.
    pub async fn ok(&self, req: Request) -> Result<(), ClientError> {
        match self.request(req).await? {
            Reply::Ok => Ok(()),
            Reply::Error { message } => Err(ClientError::Daemon(message)),
            other => Err(ClientError::UnexpectedReply {
                request: "ok".into(),
                reply: format!("{other:?}"),
            }),
        }
    }

    pub fn is_connected(&self) -> bool {
        todo!("Client::is_connected")
    }

    /// Stop the reader/reconnect loop and close the socket.
    pub async fn close(self) {
        todo!("Client::close")
    }
}
