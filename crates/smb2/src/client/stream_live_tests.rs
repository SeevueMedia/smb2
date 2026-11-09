use super::*;
use crate::client::test_helpers::{
    build_compound_response_frame, build_query_info_error_response, build_query_info_response,
};
use crate::msg::{header::Header, query_info::QueryInfoRequest};
use crate::types::SessionId;

fn version_answer(
    size: u64,
    modified: u64,
    changed: u64,
    identity: Option<FileIdentity>,
) -> Vec<u8> {
    let mut basic = vec![0u8; 40];
    basic[16..24].copy_from_slice(&modified.to_le_bytes());
    basic[24..32].copy_from_slice(&changed.to_le_bytes());
    let mut standard = vec![0u8; 24];
    standard[8..16].copy_from_slice(&size.to_le_bytes());
    let (index, volume) = match identity {
        Some(id) => {
            let mut volume = vec![0u8; 18];
            volume[8..12].copy_from_slice(&id.volume_serial.to_le_bytes());
            (
                build_query_info_response(id.index_number.to_le_bytes().to_vec()),
                build_query_info_response(volume),
            )
        }
        None => (
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
        ),
    };
    build_compound_response_frame(&[
        build_query_info_response(basic),
        build_query_info_response(standard),
        index,
        volume,
    ])
}

#[tokio::test]
async fn live_version_checks_the_original_handle_and_all_version_fields() {
    let id = FileIdentity {
        index_number: 77,
        volume_serial: 99,
    };
    for answer in [
        version_answer(8, 0, 0, Some(id)),
        version_answer(8, 0, 1, Some(id)),
        version_answer(8, 1, 0, Some(id)),
        version_answer(9, 0, 0, Some(id)),
        version_answer(
            8,
            0,
            0,
            Some(FileIdentity {
                index_number: 78,
                ..id
            }),
        ),
        version_answer(8, 0, 0, None),
    ]
    .into_iter()
    .enumerate()
    {
        let (case, answer) = answer;
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_reader_open_response(
            build_create_response(test_file_id(), 8),
            Some(id),
            None,
            None,
        ));
        mock.queue_response(answer);
        mock.queue_response(build_close_response());
        let reader = test_tree()
            .open_file_reader(setup_connection(&mock), "original.bin")
            .await
            .unwrap();
        let result = reader.validate_version().await;
        if case == 0 {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().kind(),
                crate::ErrorKind::FileChanged,
                "case {case}"
            );
        }
        // Every query names the reader's existing file ID, with no CREATE or path lookup.
        let sent = mock.sent_message(1).unwrap();
        let mut offset = 0;
        let mut classes = Vec::new();
        loop {
            let mut cursor = ReadCursor::new(&sent[offset..]);
            let header = Header::unpack(&mut cursor).unwrap();
            assert_eq!(header.command, Command::QueryInfo);
            let request = QueryInfoRequest::unpack(&mut cursor).unwrap();
            assert_eq!(request.file_id, test_file_id());
            classes.push(request.file_info_class);
            if header.next_command == 0 {
                break;
            }
            offset += header.next_command as usize;
        }
        assert_eq!(classes, [4, 5, 6, 1]);
        reader.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_version_refuses_missing_metadata_and_retries_on_the_same_handle() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(reader_opened(test_file_id(), 8));
    mock.queue_response(build_compound_response_frame(
        &vec![build_query_info_error_response(NtStatus::ACCESS_DENIED); 4],
    ));
    mock.queue_response(version_answer(8, 0, 0, None));
    mock.queue_response(build_close_response());
    let reader = test_tree()
        .open_file_reader(setup_connection(&mock), "file.bin")
        .await
        .unwrap();
    assert_eq!(
        reader.validate_version().await.unwrap_err().kind(),
        crate::ErrorKind::AccessDenied
    );
    reader.validate_version().await.unwrap();
    reader.close().await.unwrap();
}

#[tokio::test]
async fn live_version_never_queries_a_replacement_session() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(reader_opened(test_file_id(), 8));
    let mut conn = setup_connection(&mock);
    let reader = test_tree()
        .open_file_reader(conn.clone(), "file.bin")
        .await
        .unwrap();
    conn.set_session_id(SessionId(999));
    assert!(matches!(
        reader.validate_version().await,
        Err(Error::HandleInvalidated { .. })
    ));
    assert_eq!(mock.sent_count(), 1);
    reader.close().await.unwrap();
}
