use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{KexError, Result};

const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MB

pub async fn write_message<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    msg: &T,
) -> Result<()> {
    let json = serde_json::to_vec(msg)?;
    let len = (json.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&json).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read_message<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(KexError::Ipc(format!("message too large: {len} bytes")));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::message::{Request, Response};
    use tokio::net::{UnixListener, UnixStream};

    async fn paired_streams() -> (UnixStream, UnixStream) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let client = UnixStream::connect(&sock).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn roundtrip_request() {
        let (mut client, mut server) = paired_streams().await;
        let req = Request::TerminalCreate {
            name: Some("test".into()),
        };
        write_message(&mut client, &req).await.unwrap();
        let decoded: Request = read_message(&mut server).await.unwrap();
        assert!(matches!(decoded, Request::TerminalCreate { name: Some(n) } if n == "test"));
    }

    #[tokio::test]
    async fn roundtrip_response() {
        let (mut client, mut server) = paired_streams().await;
        let resp = Response::TerminalCreated {
            id: "abc123".into(),
        };
        write_message(&mut server, &resp).await.unwrap();
        let decoded: Response = read_message(&mut client).await.unwrap();
        assert!(matches!(decoded, Response::TerminalCreated { id } if id == "abc123"));
    }

    #[tokio::test]
    async fn roundtrip_view_update_layout() {
        let (mut client, mut server) = paired_streams().await;
        let req = Request::ViewUpdateLayout {
            view_id: "v1".into(),
            layout: serde_json::json!({"type": "leaf", "terminal_id": "t1"}),
            focused: "t1".into(),
        };
        write_message(&mut client, &req).await.unwrap();
        let decoded: Request = read_message(&mut server).await.unwrap();
        assert!(matches!(decoded, Request::ViewUpdateLayout { view_id, focused, .. } if view_id == "v1" && focused == "t1"));
    }

    #[tokio::test]
    async fn roundtrip_view_remove_terminal() {
        let (mut client, mut server) = paired_streams().await;
        let req = Request::ViewRemoveTerminal {
            view_id: "v1".into(),
            terminal_id: "t1".into(),
        };
        write_message(&mut client, &req).await.unwrap();
        let decoded: Request = read_message(&mut server).await.unwrap();
        assert!(matches!(decoded, Request::ViewRemoveTerminal { view_id, terminal_id } if view_id == "v1" && terminal_id == "t1"));
    }

    #[tokio::test]
    async fn roundtrip_view_attach_with_layout() {
        let (mut client, mut server) = paired_streams().await;
        let resp = Response::ViewAttach {
            terminal_ids: vec!["t1".into(), "t2".into()],
            layout: Some(serde_json::json!({"type": "split"})),
            focused: Some("t2".into()),
        };
        write_message(&mut server, &resp).await.unwrap();
        let decoded: Response = read_message(&mut client).await.unwrap();
        assert!(matches!(decoded, Response::ViewAttach { terminal_ids, layout: Some(_), focused: Some(f) } if terminal_ids.len() == 2 && f == "t2"));
    }
}
