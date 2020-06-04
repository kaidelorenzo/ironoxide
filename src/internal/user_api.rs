use crate::{
    crypto::aes::{self, EncryptedMasterKey},
    internal::{rest::IronCoreRequest, *},
};
use chrono::{DateTime, Utc};
use itertools::{Either, Itertools};
use rand::rngs::OsRng;
use recrypt::prelude::*;
use std::{
    collections::HashMap,
    convert::{TryFrom, TryInto},
    result::Result,
    sync::Mutex,
};

/// private module that handles interaction with the IronCore webservice
mod requests;

/// ID of a user.
///
/// The ID can be validated from a `String` or `&str` using `UserId::try_from`.
///
/// # Requirements
/// - Must be unique within the user's segment.
/// - Must match the regex `^[a-zA-Z0-9_.$#|@/:;=+'-]+$`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct UserId(pub(crate) String);
impl UserId {
    /// Constructs a `UserId` with no validation. Useful for IDs coming back from the web service.
    pub fn unsafe_from_string(id: String) -> UserId {
        UserId(id)
    }
    /// ID of the user
    pub fn id(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for UserId {
    type Error = IronOxideErr;
    fn try_from(user_id: String) -> Result<Self, Self::Error> {
        user_id.as_str().try_into()
    }
}
impl TryFrom<&str> for UserId {
    type Error = IronOxideErr;
    fn try_from(user_id: &str) -> Result<Self, Self::Error> {
        validate_id(user_id, "user_id").map(UserId)
    }
}

/// ID of a device.
///
/// The ID can be validated from a `u64` using `DeviceId::try_from`.
///
/// # Requirements
/// - Must be greater than 0.
/// - Must be less than or equal to `i64::max_value()`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct DeviceId(pub(crate) u64);
impl DeviceId {
    /// ID of the device
    pub fn id(&self) -> &u64 {
        &self.0
    }
}
impl TryFrom<u64> for DeviceId {
    type Error = IronOxideErr;
    fn try_from(device_id: u64) -> Result<Self, Self::Error> {
        // Validate the range of the device ID to always be positive, but also be
        // less than i64 (i.e. no high bit set) for compatibility with other
        // languages (i.e. Java)
        if device_id < 1 || device_id > (i64::max_value() as u64) {
            Err(IronOxideErr::ValidationError(
                "device_id".to_string(),
                format!("'{}' must be a number greater than 0", device_id),
            ))
        } else {
            Ok(DeviceId(device_id))
        }
    }
}

/// Name of a device.
///
/// The name can be validated from a `String` or `&str` using `DeviceName::try_from`.
///
/// # Requirements
/// - Must be between 1 and 100 characters long.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceName(pub(crate) String);
impl DeviceName {
    /// Name of the device
    pub fn name(&self) -> &String {
        &self.0
    }
}
impl TryFrom<String> for DeviceName {
    type Error = IronOxideErr;
    fn try_from(device_name: String) -> Result<Self, Self::Error> {
        device_name.as_str().try_into()
    }
}
impl TryFrom<&str> for DeviceName {
    type Error = IronOxideErr;
    fn try_from(name: &str) -> Result<Self, Self::Error> {
        validate_name(name, "device_name").map(DeviceName)
    }
}

/// Metadata for a newly created user.
///
/// Includes the user's public key and whether the user's private key needs rotation.
///
/// Result from [user_create](trait.UserOps.html#tymethod.user_create).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserCreateResult {
    user_public_key: PublicKey,
    needs_rotation: bool,
}

impl UserCreateResult {
    /// Public key for the user
    ///
    /// For most use cases, this public key can be discarded, as IronCore escrows the user's keys. The escrowed keys are unlocked
    /// by the provided password.
    pub fn user_public_key(&self) -> &PublicKey {
        &self.user_public_key
    }
    /// Whether the user's private key needs to be rotated
    pub fn needs_rotation(&self) -> bool {
        self.needs_rotation
    }
}

/// Public and private key pair used for document encryption and decryption.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeyPair {
    public_key: PublicKey,
    private_key: PrivateKey,
}
impl KeyPair {
    /// Constructs a new `KeyPair` from the `recrypt` versions of the public and private keys.
    pub fn new(public_key: RecryptPublicKey, private_key: RecryptPrivateKey) -> Self {
        KeyPair {
            public_key: public_key.into(),
            private_key: private_key.into(),
        }
    }
    /// Public key of the user
    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }
    /// Private key of the user
    pub fn private_key(&self) -> &PrivateKey {
        &self.private_key
    }
}

/// Bundle of information for adding a device
pub(crate) struct DeviceAdd {
    /// Public key of the user
    user_public_key: PublicKey,
    /// Transform key from the user's private key to the device's public key
    transform_key: TransformKey,
    /// Public/private encryption key pair for the device
    device_keys: KeyPair,
    /// Signing key pair for the device, used for authorized device requests
    signing_keys: DeviceSigningKeyPair,
    /// Signature needed for authorized device requests
    signature: SchnorrSignature,
    /// Timestamp used in the schnorr signature
    signature_ts: DateTime<Utc>,
}

/// Metadata for a user.
///
/// Result from [user_verify](trait.UserOps.html#tymethod.user_verify).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserResult {
    account_id: UserId,
    segment_id: usize,
    user_public_key: PublicKey,
    needs_rotation: bool,
}
impl UserResult {
    /// ID of the user
    pub fn account_id(&self) -> &UserId {
        &self.account_id
    }
    /// Public key of the user
    pub fn user_public_key(&self) -> &PublicKey {
        &self.user_public_key
    }
    /// Segment ID for the user
    pub fn segment_id(&self) -> usize {
        self.segment_id
    }
    /// Whether the user's private key needs rotation
    pub fn needs_rotation(&self) -> bool {
        self.needs_rotation
    }
}

/// Metadata for each device the user has authorized.
///
/// The results are sorted based on the device's ID.
///
/// Result from [user_list_devices](trait.UserOps.html#tymethod.user_list_devices).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserDeviceListResult {
    result: Vec<UserDevice>,
}
impl UserDeviceListResult {
    fn new(result: Vec<UserDevice>) -> UserDeviceListResult {
        UserDeviceListResult { result }
    }
    /// Metadata for each device the user has authorized
    pub fn result(&self) -> &Vec<UserDevice> {
        &self.result
    }
}

/// Metadata for a device.
///
/// Result from [`UserDeviceListResult.result()](struct.UserDeviceListResult.html#method.result).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserDevice {
    id: DeviceId,
    name: Option<DeviceName>,
    /// time the device was created
    created: DateTime<Utc>,
    /// time the device was last updated
    last_updated: DateTime<Utc>,
    /// true if this UserDevice is the device making the query
    is_current_device: bool,
}
impl UserDevice {
    /// ID of the device
    pub fn id(&self) -> &DeviceId {
        &self.id
    }
    /// Name of the device
    pub fn name(&self) -> Option<&DeviceName> {
        self.name.as_ref()
    }
    /// Date and time when the device was created
    pub fn created(&self) -> &DateTime<Utc> {
        &self.created
    }
    /// Date and time when the device was last updated
    pub fn last_updated(&self) -> &DateTime<Utc> {
        &self.last_updated
    }
    /// Whether this is the device that was used to make the API request
    pub fn is_current_device(&self) -> bool {
        self.is_current_device
    }
}

/// Verify an existing user given a valid JWT.
pub async fn user_verify(
    jwt: Jwt,
    request: IronCoreRequest,
) -> Result<Option<UserResult>, IronOxideErr> {
    requests::user_verify::user_verify(&jwt, &request)
        .await?
        .map(|resp| resp.try_into())
        .transpose()
}

/// Create a user
pub async fn user_create<CR: rand::CryptoRng + rand::RngCore>(
    recrypt: &Recrypt<Sha256, Ed25519, RandomBytes<CR>>,
    jwt: Jwt,
    passphrase: Password,
    needs_rotation: bool,
    request: IronCoreRequest,
) -> Result<UserCreateResult, IronOxideErr> {
    let (encrypted_priv_key, recrypt_pub) = recrypt
        .generate_key_pair()
        .map_err(IronOxideErr::from)
        .and_then(|(recrypt_priv, recrypt_pub)| {
            Ok(aes::encrypt_user_master_key(
                &Mutex::new(rand::thread_rng()),
                passphrase.0.as_str(),
                recrypt_priv.bytes(),
            )
            .map(|encrypted_private_key| (encrypted_private_key, recrypt_pub))?)
        })?;

    requests::user_create::user_create(
        &jwt,
        recrypt_pub.into(),
        encrypted_priv_key.into(),
        needs_rotation,
        request,
    )
    .await?
    .try_into()
}

/// A user's encrypted private key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EncryptedPrivateKey(Vec<u8>);
impl EncryptedPrivateKey {
    /// The bytes of the user's encrypted private key
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Metadata from user private key rotation.
///
/// Result from [user_rotate_private_key](trait.UserOps.html#tymethod.user_rotate_private_key).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UserUpdatePrivateKeyResult {
    user_master_private_key: EncryptedPrivateKey,
    needs_rotation: bool,
}
impl UserUpdatePrivateKeyResult {
    /// Updated encrypted private key of the user
    pub fn user_master_private_key(&self) -> &EncryptedPrivateKey {
        &self.user_master_private_key
    }
    /// Whether this user's private key needs further rotation
    pub fn needs_rotation(&self) -> bool {
        self.needs_rotation
    }
}

/// Get metadata about the current user
pub async fn user_get_current(auth: &RequestAuth) -> Result<UserResult, IronOxideErr> {
    requests::user_get::get_curr_user(auth)
        .await
        .and_then(|result| {
            Ok(UserResult {
                needs_rotation: result.needs_rotation,
                user_public_key: result.user_master_public_key.try_into()?,
                segment_id: result.segment_id,
                account_id: result.id,
            })
        })
}

/// Rotate the user's private key. The public key for the user remains unchanged.
pub async fn user_rotate_private_key<CR: rand::CryptoRng + rand::RngCore>(
    recrypt: &Recrypt<Sha256, Ed25519, RandomBytes<CR>>,
    password: Password,
    auth: &RequestAuth,
) -> Result<UserUpdatePrivateKeyResult, IronOxideErr> {
    let requests::user_get::CurrentUserResponse {
        user_private_key: encrypted_priv_key,
        current_key_id,
        id: curr_user_id,
        ..
    } = requests::user_get::get_curr_user(auth).await?;
    let (user_id, curr_key_id, new_encrypted_priv_key, aug_factor) = {
        let priv_key: PrivateKey = aes::decrypt_user_master_key(
            &password.0,
            &aes::EncryptedMasterKey::new_from_slice(&encrypted_priv_key.0)?,
        )?
        .into();

        let (new_priv_key, aug_factor) = augment_private_key_with_retry(recrypt, &priv_key)?;
        let new_encrypted_priv_key = aes::encrypt_user_master_key(
            &Mutex::new(OsRng::default()),
            &password.0,
            new_priv_key.as_bytes(),
        )?;
        (
            curr_user_id,
            current_key_id,
            new_encrypted_priv_key,
            aug_factor,
        )
    };
    Ok(requests::user_update_private_key::update_private_key(
        auth,
        user_id,
        curr_key_id,
        new_encrypted_priv_key.into(),
        aug_factor.into(),
    )
    .await?
    .into())
}

/// Metadata for a newly created device.
///
/// Can be converted into a `DeviceContext` with `DeviceContext::from`.
///
/// Result from [generate_new_device](trait.UserOps.html#tymethod.generate_new_device).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DeviceAddResult {
    account_id: UserId,
    segment_id: usize,
    device_private_key: PrivateKey,
    signing_private_key: DeviceSigningKeyPair,
    device_id: DeviceId,
    name: Option<DeviceName>,
    created: DateTime<Utc>,
    last_updated: DateTime<Utc>,
}
impl DeviceAddResult {
    /// ID of the device
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }
    /// Name of the device
    pub fn name(&self) -> Option<&DeviceName> {
        self.name.as_ref()
    }
    /// ID of the user who owns the device
    pub fn account_id(&self) -> &UserId {
        &self.account_id
    }
    /// Segment of the user
    pub fn segment_id(&self) -> usize {
        self.segment_id
    }
    /// The signing key pair for the device
    pub fn signing_private_key(&self) -> &DeviceSigningKeyPair {
        &self.signing_private_key
    }
    /// Private encryption key of the device
    ///
    /// This is different from the user's private key.
    pub fn device_private_key(&self) -> &PrivateKey {
        &self.device_private_key
    }
    /// The date and time when the device was created
    pub fn created(&self) -> &DateTime<Utc> {
        &self.created
    }
    /// The date and time when the device was last updated
    pub fn last_updated(&self) -> &DateTime<Utc> {
        &self.last_updated
    }
}
impl From<DeviceAddResult> for DeviceContext {
    fn from(dar: DeviceAddResult) -> Self {
        DeviceContext::new(
            dar.account_id,
            dar.segment_id,
            dar.device_private_key,
            dar.signing_private_key,
        )
    }
}

/// Generate a device key for the user specified in the JWT.
pub async fn generate_device_key<CR: rand::CryptoRng + rand::RngCore>(
    recrypt: &Recrypt<Sha256, Ed25519, RandomBytes<CR>>,
    jwt: &Jwt,
    password: Password,
    device_name: Option<DeviceName>,
    signing_ts: &DateTime<Utc>,
    request: &IronCoreRequest,
) -> Result<DeviceAddResult, IronOxideErr> {
    // verify that this user exists
    let requests::user_verify::UserVerifyResponse {
        user_private_key,
        user_master_public_key,
        id: account_id,
        segment_id,
        ..
    } = requests::user_verify::user_verify(jwt, request)
        .await?
        .ok_or_else(|| {
            IronOxideErr::UserDoesNotExist(
                "Device cannot be added to a user that doesn't exist".to_string(),
            )
        })?;
    // unpack the verified user and create a DeviceAdd
    let (device_add, account_id) = (
        {
            let user_public_key: RecryptPublicKey =
                PublicKey::try_from(user_master_public_key)?.into();
            let user_private_key = EncryptedMasterKey::new_from_slice(&user_private_key.0)?;

            // decrypt the user's master key using the provided password
            let user_private_key = aes::decrypt_user_master_key(&password.0, &user_private_key)?;

            let user_keypair: KeyPair =
                KeyPair::new(user_public_key, RecryptPrivateKey::new(user_private_key));

            // generate info needed to add a device
            generate_device_add(recrypt, jwt, &user_keypair, signing_ts)?
        },
        account_id.try_into()?,
    );

    // call device_add
    let device_add_response =
        requests::device_add::user_device_add(jwt, &device_add, &device_name, request).await?;
    // on successful response, assemble a DeviceContext for the caller
    Ok(DeviceAddResult {
        account_id,
        segment_id,
        device_private_key: device_add.device_keys.private_key,
        signing_private_key: device_add.signing_keys,
        device_id: device_add_response.device_id,
        name: device_add_response.name,
        created: device_add_response.created,
        last_updated: device_add_response.updated,
    })
}

pub async fn device_list(auth: &RequestAuth) -> Result<UserDeviceListResult, IronOxideErr> {
    let resp = requests::device_list::device_list(auth).await?;
    let devices = {
        let mut vec: Vec<UserDevice> = resp.result.into_iter().map(UserDevice::from).collect();
        // sort the devices by device_id
        vec.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        vec
    };
    Ok(UserDeviceListResult::new(devices))
}

pub async fn device_delete(
    auth: &RequestAuth,
    device_id: Option<&DeviceId>,
) -> Result<DeviceId, IronOxideErr> {
    match device_id {
        Some(device_id) => requests::device_delete::device_delete(auth, device_id).await,
        None => requests::device_delete::device_delete_current(auth).await,
    }
    .map(|resp| resp.id)
}

/// Get a list of users public keys given a list of user account IDs
pub async fn user_key_list(
    auth: &RequestAuth,
    user_ids: &Vec<UserId>,
) -> Result<HashMap<UserId, PublicKey>, IronOxideErr> {
    requests::user_key_list::user_key_list_request(auth, user_ids)
        .await
        .map(
            move |requests::user_key_list::UserKeyListResponse { result }| {
                result
                    .into_iter()
                    .fold(HashMap::with_capacity(user_ids.len()), |mut acc, user| {
                        let maybe_pub_key =
                            PublicKey::try_from(user.user_master_public_key.clone());
                        maybe_pub_key.into_iter().for_each(|pub_key| {
                            //We asked the api for valid user ids. We're assuming here that the response has valid user ids.
                            acc.insert(UserId::unsafe_from_string(user.id.clone()), pub_key);
                        });
                        acc
                    })
            },
        )
}

/// Get the keys for users. The result should be either a failure for a specific UserId (Left) or the id with their public key (Right).
/// The resulting lists will have the same combined size as the incoming list.
/// Calling this with an empty `users` list will not result in a call to the server.
pub(crate) async fn get_user_keys(
    auth: &RequestAuth,
    users: &Vec<UserId>,
) -> Result<(Vec<UserId>, Vec<WithKey<UserId>>), IronOxideErr> {
    // if there aren't any users in the list, just return with empty results
    if users.is_empty() {
        Ok((vec![], vec![]))
    } else {
        user_api::user_key_list(auth, users)
            .await
            .map(|ids_with_keys| {
                users.clone().into_iter().partition_map(|user_id| {
                    let maybe_public_key = ids_with_keys.get(&user_id).cloned();
                    match maybe_public_key {
                        Some(pk) => Either::Right(WithKey::new(user_id, pk)),
                        None => Either::Left(user_id),
                    }
                })
            })
    }
}

/// Generate all the necessary device keys, transform keys, and signatures to be able to add a new user device.
/// Specifically, it creates a device key pair and signing key pair, then a transform key between the provided
/// user private key and device public key. Also generated is a device add signature that is necessary to hit the API.
fn generate_device_add<CR: rand::CryptoRng + rand::RngCore>(
    recrypt: &Recrypt<Sha256, Ed25519, RandomBytes<CR>>,
    jwt: &Jwt,
    user_master_keypair: &KeyPair,
    signing_ts: &DateTime<Utc>,
) -> Result<DeviceAdd, IronOxideErr> {
    let signing_keypair = recrypt.generate_ed25519_key_pair();
    let (recrypt_priv_key, recrypt_pub_key) = recrypt.generate_key_pair()?;
    let device_keypair = KeyPair::new(recrypt_pub_key, recrypt_priv_key);

    // generate a transform key from the user's private key to the new device
    let trans_key: TransformKey = recrypt
        .generate_transform_key(
            user_master_keypair.private_key().recrypt_key(),
            &device_keypair.public_key().into(),
            &signing_keypair,
        )?
        .into();

    let sig = gen_device_add_signature(recrypt, jwt, user_master_keypair, &trans_key, signing_ts);
    Ok(DeviceAdd {
        user_public_key: user_master_keypair.public_key().clone(),
        transform_key: trans_key,
        device_keys: device_keypair,
        signing_keys: signing_keypair.into(),
        signature: sig,
        signature_ts: signing_ts.to_owned(),
    })
}

/// Generate a schnorr signature for calling the device add endpoint in the IronCore service
fn gen_device_add_signature<CR: rand::CryptoRng + rand::RngCore>(
    recrypt: &Recrypt<Sha256, Ed25519, RandomBytes<CR>>,
    jwt: &Jwt,
    user_master_keypair: &KeyPair,
    transform_key: &TransformKey,
    signing_ts: &DateTime<Utc>,
) -> SchnorrSignature {
    struct SignedMessage<'a> {
        timestamp: &'a DateTime<Utc>,
        transform_key: &'a TransformKey,
        jwt: &'a Jwt,
        user_public_key: &'a PublicKey,
    };

    impl<'a> recrypt::api::Hashable for SignedMessage<'a> {
        fn to_bytes(&self) -> Vec<u8> {
            let mut vec: Vec<u8> = vec![];
            vec.extend_from_slice(self.timestamp.timestamp_millis().to_string().as_bytes());
            vec.extend_from_slice(&self.transform_key.to_bytes());
            vec.extend_from_slice(&self.jwt.to_utf8());
            vec.extend_from_slice(&self.user_public_key.as_bytes());
            vec
        }
    }

    let msg = SignedMessage {
        timestamp: signing_ts,
        transform_key,
        jwt,
        user_public_key: user_master_keypair.public_key(),
    };

    recrypt
        .schnorr_sign(
            user_master_keypair.private_key().recrypt_key(),
            &user_master_keypair.public_key().into(),
            &msg,
        )
        .into()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use galvanic_assert::*;

    pub fn create_user_result(
        account_id: UserId,
        segment_id: usize,
        user_public_key: PublicKey,
        needs_rotation: bool,
    ) -> UserResult {
        UserResult {
            account_id,
            segment_id,
            user_public_key,
            needs_rotation,
        }
    }

    #[test]
    fn user_id_validate_good() {
        let user_id1 = "a_fo_real_good_group_id$";
        let user_id2 = "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789";
        assert_eq!(
            UserId(user_id1.to_string()),
            UserId::try_from(user_id1).unwrap()
        );
        assert_eq!(
            UserId(user_id2.to_string()),
            UserId::try_from(user_id2).unwrap()
        )
    }

    #[test]
    fn user_id_rejects_invalid() {
        let user_id1 = UserId::try_from("not a good ID!");
        let user_id2 = UserId::try_from("!!");
        let user_id3 = UserId::try_from("01234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567891");

        assert_that!(
            &user_id1.unwrap_err(),
            is_variant!(IronOxideErr::ValidationError)
        );
        assert_that!(
            &user_id2.unwrap_err(),
            is_variant!(IronOxideErr::ValidationError)
        );
        assert_that!(
            &user_id3.unwrap_err(),
            is_variant!(IronOxideErr::ValidationError)
        );
    }

    #[test]
    fn user_id_rejects_empty() {
        let user_id = UserId::try_from("");
        assert_that!(&user_id, is_variant!(Err));
        assert_that!(
            &user_id.unwrap_err(),
            is_variant!(IronOxideErr::ValidationError)
        );

        let user_id = UserId::try_from("\n \t  ");
        assert_that!(&user_id, is_variant!(Err));
        assert_that!(
            &user_id.unwrap_err(),
            is_variant!(IronOxideErr::ValidationError)
        );
    }
}