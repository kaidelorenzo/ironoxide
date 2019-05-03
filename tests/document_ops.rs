mod common;
use common::{create_second_user, init_sdk};
use galvanic_assert::matchers::collection::*;
use ironoxide::{document::*, prelude::*};
use std::convert::{TryFrom, TryInto};

#[cfg(test)]
#[macro_use]
extern crate galvanic_assert;

#[macro_use]
extern crate serde_json;

#[test]
fn doc_create_without_id() {
    let mut sdk = init_sdk();

    let doc = [0u8; 64];

    let doc_result = sdk.document_encrypt(&doc, &Default::default());

    assert!(doc_result.is_ok());
    let doc_result = doc_result.unwrap();
    assert_eq!(doc_result.grants().len(), 1); // access always granted to creator
    assert_eq!(doc_result.access_errs().len(), 0);
}
#[test]
fn doc_create_with_grant() {
    let mut sdk = init_sdk();

    let doc = [0u8; 64];

    let bad_user: UserId = "bad_user".try_into().unwrap();
    let bad_group: GroupId = "bad_group".try_into().unwrap();

    let doc_result = sdk
        .document_encrypt(
            &doc,
            &DocumentEncryptOpts::new(
                None,
                Some("first name".try_into().unwrap()),
                vec![
                    UserOrGroup::User {
                        id: bad_user.clone(),
                    },
                    UserOrGroup::Group {
                        id: bad_group.clone(),
                    },
                ],
            ),
        )
        .unwrap();

    assert_eq!(doc_result.grants().len(), 1);
    assert_eq!(
        doc_result.grants()[0],
        UserOrGroup::User {
            id: sdk.device().account_id().clone()
        }
    );
    assert_eq!(doc_result.access_errs().len(), 2);
    assert_that!(
        &doc_result
            .access_errs()
            .iter()
            .map(|err| err.user_or_group.clone())
            .collect::<Vec<_>>(),
        contains_in_any_order(vec![
            UserOrGroup::User { id: bad_user },
            UserOrGroup::Group { id: bad_group }
        ])
    )
}
#[test]
fn doc_create_and_adjust_name() {
    let mut sdk = init_sdk();

    let doc = [0u8; 64];

    let doc_result = sdk
        .document_encrypt(
            &doc,
            &DocumentEncryptOpts::new(
                None,
                Some("first name".try_into().unwrap()),
                vec![UserOrGroup::User {
                    id: UserId::try_from("bad-user").expect("should be good id"),
                }],
            ),
        )
        .unwrap();

    assert_eq!(doc_result.name().unwrap().name(), &"first name".to_string());

    let first_update = sdk
        .document_update_name(&doc_result.id(), Some(&"second name".try_into().unwrap()))
        .unwrap();

    assert_eq!(
        first_update.name().unwrap().name(),
        &"second name".to_string()
    );

    let last_update = sdk.document_update_name(&doc_result.id(), None).unwrap();

    assert!(last_update.name().is_none());
}

#[test]
fn doc_decrypt_roundtrip() {
    let mut sdk = init_sdk();
    let doc = [43u8; 64];
    let encrypted_doc = sdk.document_encrypt(&doc, &Default::default()).unwrap();

    sdk.document_get_metadata(&encrypted_doc.id()).unwrap();

    let decrypted = sdk
        .document_decrypt(&encrypted_doc.encrypted_data())
        .unwrap();

    assert_eq!(doc.to_vec(), decrypted.decrypted_data());
}

#[test]
fn doc_encrypt_update_and_decrypt() {
    let mut sdk = init_sdk();
    let doc1 = [20u8; 72];

    let encrypted_doc = sdk.document_encrypt(&doc1, &Default::default()).unwrap();

    let doc_id = &encrypted_doc.id();

    let doc2 = [10u8; 11];

    let updated_encrypted_doc = sdk.document_update_bytes(doc_id, &doc2).unwrap();

    let decrypted = sdk
        .document_decrypt(&updated_encrypted_doc.encrypted_data())
        .unwrap();

    assert_eq!(doc2.to_vec(), decrypted.decrypted_data());
}

#[test]
fn doc_grant_access() {
    let mut sdk = init_sdk();

    let doc = [0u8; 64];
    let doc_result = sdk.document_encrypt(&doc, &Default::default());
    assert!(doc_result.is_ok());
    let doc_id = doc_result.unwrap().id().clone();

    // create a second user to grant access to the document
    let user = create_second_user();

    let group_result = sdk.group_create(&Default::default());
    assert!(group_result.is_ok());
    let group_id = group_result.unwrap().id().clone();

    let grant_result = sdk.document_grant_access(
        &doc_id,
        &vec![
            UserOrGroup::User {
                id: user.account_id().clone(),
            },
            UserOrGroup::Group { id: group_id },
            UserOrGroup::User {
                id: "bad-user-id".try_into().unwrap(),
            },
            UserOrGroup::Group {
                id: "bad-group-id".try_into().unwrap(),
            },
        ],
    );
    dbg!(&grant_result);
    assert!(grant_result.is_ok());
    let grants = grant_result.unwrap();
    assert_eq!(2, grants.succeeded().len());
    assert_eq!(2, grants.failed().len());
}

#[test]
fn doc_revoke_access() {
    let mut sdk = init_sdk();

    let doc = [0u8; 64];
    let doc_result = sdk.document_encrypt(&doc, &Default::default());
    assert!(doc_result.is_ok());
    let doc_id = doc_result.unwrap().id().clone();

    // create a second user to grant/revoke access to the document
    let user = create_second_user();

    let group_result = sdk.group_create(&Default::default());
    assert!(group_result.is_ok());
    let group_id = group_result.unwrap().id().clone();

    let grant_result = sdk.document_grant_access(
        &doc_id,
        &vec![
            UserOrGroup::User {
                id: user.account_id().clone(),
            },
            UserOrGroup::Group {
                id: group_id.clone(),
            },
        ],
    );

    assert!(grant_result.is_ok());
    let grants = grant_result.unwrap();
    assert_eq!(grants.succeeded().len(), 2);

    let revoke_result = sdk.document_revoke_access(
        &doc_id,
        &vec![
            UserOrGroup::User {
                id: user.account_id().clone(),
            },
            UserOrGroup::Group {
                id: group_id.clone(),
            },
            UserOrGroup::User {
                id: "bad-user-id".try_into().unwrap(),
            },
            UserOrGroup::Group {
                id: "bad-group-id".try_into().unwrap(),
            },
        ],
    );

    assert!(revoke_result.is_ok());
    let revokes = revoke_result.unwrap();
    assert_eq!(revokes.succeeded().len(), 2);
    assert_eq!(revokes.failed().len(), 2)
}