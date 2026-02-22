use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{KexError, Result};
use crate::ipc::message::BinaryFrame;

const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MB
const FRAME_TYPE_DATA: u8 = 0x01;
const FRAME_TYPE_RESIZE: u8 = 0x02;
const FRAME_TYPE_DETACH: u8 = 0x03;
const FRAME_TYPE_CONTROL: u8 = 0x10;

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

pub async fn write_binary_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    terminal_id: &str,
    frame: &BinaryFrame,
) -> Result<()> {
    let type_byte = match frame {
        BinaryFrame::Data(_) => FRAME_TYPE_DATA,
        BinaryFrame::Resize { .. } => FRAME_TYPE_RESIZE,
        BinaryFrame::Detach => FRAME_TYPE_DETACH,
        BinaryFrame::Control(_) => FRAME_TYPE_CONTROL,
    };

    // Header: [1B type][8B tid][4B len]
    let mut header = [0u8; 13];
    header[0] = type_byte;
    let tid_bytes = terminal_id.as_bytes();
    debug_assert!(
        tid_bytes.len() <= 8,
        "terminal_id exceeds 8-byte frame field: {terminal_id}"
    );
    let copy_len = tid_bytes.len().min(8);
    header[1..1 + copy_len].copy_from_slice(&tid_bytes[..copy_len]);

    match frame {
        BinaryFrame::Data(data) => {
            let len = (data.len() as u32).to_be_bytes();
            header[9..13].copy_from_slice(&len);
            stream.write_all(&header).await?;
            stream.write_all(data).await?;
        }
        BinaryFrame::Resize { cols, rows } => {
            header[9..13].copy_from_slice(&4u32.to_be_bytes());
            stream.write_all(&header).await?;
            stream.write_all(&cols.to_be_bytes()).await?;
            stream.write_all(&rows.to_be_bytes()).await?;
        }
        BinaryFrame::Detach => {
            // len = 0
            stream.write_all(&header).await?;
        }
        BinaryFrame::Control(payload) => {
            let len = (payload.len() as u32).to_be_bytes();
            header[9..13].copy_from_slice(&len);
            stream.write_all(&header).await?;
            stream.write_all(payload).await?;
        }
    }
    stream.flush().await?;
    Ok(())
}

pub async fn read_binary_frame(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<(String, BinaryFrame)> {
    let mut header = [0u8; 13];
    stream.read_exact(&mut header).await?;

    let type_byte = header[0];
    let tid = std::str::from_utf8(&header[1..9])
        .unwrap_or("")
        .trim_end_matches('\0')
        .to_string();
    let payload_len = u32::from_be_bytes([header[9], header[10], header[11], header[12]]) as usize;

    if payload_len > MAX_MESSAGE_SIZE {
        return Err(KexError::Ipc(format!(
            "frame too large: {payload_len} bytes"
        )));
    }

    let frame = match type_byte {
        FRAME_TYPE_DATA => {
            let mut buf = vec![0u8; payload_len];
            stream.read_exact(&mut buf).await?;
            BinaryFrame::Data(buf)
        }
        FRAME_TYPE_RESIZE => {
            if payload_len != 4 {
                return Err(KexError::Ipc(format!(
                    "resize frame expects 4 bytes, got {payload_len}"
                )));
            }
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await?;
            BinaryFrame::Resize {
                cols: u16::from_be_bytes([buf[0], buf[1]]),
                rows: u16::from_be_bytes([buf[2], buf[3]]),
            }
        }
        FRAME_TYPE_DETACH => BinaryFrame::Detach,
        FRAME_TYPE_CONTROL => {
            let mut buf = vec![0u8; payload_len];
            stream.read_exact(&mut buf).await?;
            BinaryFrame::Control(buf)
        }
        _ => return Err(KexError::Ipc(format!("unknown frame type: {type_byte:#x}"))),
    };

    Ok((tid, frame))
}

pub async fn write_control_frame<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    msg: &T,
) -> Result<()> {
    let payload = serde_json::to_vec(msg)?;
    write_binary_frame(stream, "\0\0\0\0\0\0\0\0", &BinaryFrame::Control(payload)).await
}

pub async fn read_control_frame<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<T> {
    let (_, frame) = read_binary_frame(stream).await?;
    match frame {
        BinaryFrame::Control(payload) => Ok(serde_json::from_slice(&payload)?),
        other => Err(KexError::Ipc(format!("expected Control frame, got {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::message::{MuxRequest, MuxResponse, Request, Response};
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
        assert!(
            matches!(decoded, Request::ViewUpdateLayout { view_id, focused, .. } if view_id == "v1" && focused == "t1")
        );
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
        assert!(
            matches!(decoded, Request::ViewRemoveTerminal { view_id, terminal_id } if view_id == "v1" && terminal_id == "t1")
        );
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
        assert!(
            matches!(decoded, Response::ViewAttach { terminal_ids, layout: Some(_), focused: Some(f) } if terminal_ids.len() == 2 && f == "t2")
        );
    }

    #[tokio::test]
    async fn binary_frame_roundtrip_data() {
        let (mut client, mut server) = paired_streams().await;
        let frame = BinaryFrame::Data(vec![1, 2, 3, 4]);
        write_binary_frame(&mut client, "abcd1234", &frame)
            .await
            .unwrap();
        let (tid, decoded) = read_binary_frame(&mut server).await.unwrap();
        assert_eq!(tid, "abcd1234");
        assert_eq!(decoded, BinaryFrame::Data(vec![1, 2, 3, 4]));
    }

    #[tokio::test]
    async fn binary_frame_roundtrip_resize() {
        let (mut client, mut server) = paired_streams().await;
        let frame = BinaryFrame::Resize {
            cols: 120,
            rows: 40,
        };
        write_binary_frame(&mut client, "term0001", &frame)
            .await
            .unwrap();
        let (tid, decoded) = read_binary_frame(&mut server).await.unwrap();
        assert_eq!(tid, "term0001");
        assert_eq!(
            decoded,
            BinaryFrame::Resize {
                cols: 120,
                rows: 40
            }
        );
    }

    #[tokio::test]
    async fn binary_frame_roundtrip_detach() {
        let (mut client, mut server) = paired_streams().await;
        write_binary_frame(&mut client, "t1234567", &BinaryFrame::Detach)
            .await
            .unwrap();
        let (tid, decoded) = read_binary_frame(&mut server).await.unwrap();
        assert_eq!(tid, "t1234567");
        assert_eq!(decoded, BinaryFrame::Detach);
    }

    #[tokio::test]
    async fn binary_frame_empty_data() {
        let (mut client, mut server) = paired_streams().await;
        let frame = BinaryFrame::Data(vec![]);
        write_binary_frame(&mut client, "abcd1234", &frame)
            .await
            .unwrap();
        let (_, decoded) = read_binary_frame(&mut server).await.unwrap();
        assert_eq!(decoded, BinaryFrame::Data(vec![]));
    }

    #[tokio::test]
    async fn binary_frame_roundtrip_control() {
        let (mut client, mut server) = paired_streams().await;
        let payload = b"{\"test\":true}".to_vec();
        let frame = BinaryFrame::Control(payload.clone());
        write_binary_frame(&mut client, "\0\0\0\0\0\0\0\0", &frame)
            .await
            .unwrap();
        let (tid, decoded) = read_binary_frame(&mut server).await.unwrap();
        assert_eq!(tid, "");
        assert_eq!(decoded, BinaryFrame::Control(payload));
    }

    #[tokio::test]
    async fn control_frame_mux_request_roundtrip() {
        let (mut client, mut server) = paired_streams().await;
        let req = MuxRequest::CreateTerminal { name: Some("dev".into()) };
        write_control_frame(&mut client, &req).await.unwrap();
        let decoded: MuxRequest = read_control_frame(&mut server).await.unwrap();
        assert!(matches!(decoded, MuxRequest::CreateTerminal { name: Some(n) } if n == "dev"));
    }

    #[tokio::test]
    async fn control_frame_mux_response_roundtrip() {
        let (mut client, mut server) = paired_streams().await;
        let resp = MuxResponse::TerminalCreated { id: "t1".into() };
        write_control_frame(&mut client, &resp).await.unwrap();
        let decoded: MuxResponse = read_control_frame(&mut server).await.unwrap();
        assert!(matches!(decoded, MuxResponse::TerminalCreated { id } if id == "t1"));
    }

    #[tokio::test]
    async fn control_frame_empty_json() {
        let (mut client, mut server) = paired_streams().await;
        let frame = BinaryFrame::Control(b"{}".to_vec());
        write_binary_frame(&mut client, "\0\0\0\0\0\0\0\0", &frame)
            .await
            .unwrap();
        let (_, decoded) = read_binary_frame(&mut server).await.unwrap();
        assert_eq!(decoded, BinaryFrame::Control(b"{}".to_vec()));
    }

    #[tokio::test]
    async fn roundtrip_multiplex_attach() {
        let (mut client, mut server) = paired_streams().await;
        let req = Request::MultiplexAttach {
            terminal_ids: vec!["t1".into(), "t2".into()],
            view_id: Some("v1".into()),
        };
        write_message(&mut client, &req).await.unwrap();
        let decoded: Request = read_message(&mut server).await.unwrap();
        assert!(matches!(decoded, Request::MultiplexAttach { terminal_ids, view_id: Some(v) } if terminal_ids.len() == 2 && v == "v1"));
    }
}
