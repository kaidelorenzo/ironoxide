use std::{fmt, num::NonZeroU32};

use rand::{self, CryptoRng, RngCore};
use ring::{aead, digest, error::Unspecified, pbkdf2};

use crate::internal::IronOxideErr;
use futures::Future;
use std::convert::TryFrom;

//There is no way this can fail. Value is most definitely not less than one.
const PBKDF2_ITERATIONS: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(250000) };
const PBKDF2_SALT_LEN: usize = 32;
const AES_GCM_TAG_LEN: usize = 16;
const AES_IV_LEN: usize = 12;
const AES_KEY_LEN: usize = 32;
//The encrypted user master key length will be the size of the encrypted key (32 bytes) plus the size of the GCM auth tag (16 bytes).
const ENCRYPTED_KEY_AND_GCM_TAG_LEN: usize = AES_KEY_LEN + AES_GCM_TAG_LEN;

pub struct EncryptedMasterKey {
    pbkdf2_salt: [u8; PBKDF2_SALT_LEN],
    aes_iv: [u8; AES_IV_LEN],
    encrypted_key: [u8; ENCRYPTED_KEY_AND_GCM_TAG_LEN],
}

impl fmt::Debug for EncryptedMasterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter
            .debug_struct(stringify!(EncryptedMasterKey))
            .field("pbkdf2_salt", &&self.pbkdf2_salt)
            .field("aes_iv", &&self.aes_iv)
            .field("encrypted_key", &&self.encrypted_key[..])
            .finish()
    }
}

impl EncryptedMasterKey {
    pub const SIZE_BYTES: usize = PBKDF2_SALT_LEN + AES_IV_LEN + ENCRYPTED_KEY_AND_GCM_TAG_LEN;

    pub fn new(
        pbkdf2_salt: [u8; PBKDF2_SALT_LEN],
        aes_iv: [u8; AES_IV_LEN],
        encrypted_key: [u8; ENCRYPTED_KEY_AND_GCM_TAG_LEN],
    ) -> EncryptedMasterKey {
        EncryptedMasterKey {
            pbkdf2_salt,
            aes_iv,
            encrypted_key,
        }
    }

    /// Construct an EncryptedMasterKey from bytes.
    /// The reciprocal of `EncryptedMasterKey::bytes`
    pub fn new_from_slice(bytes: &[u8]) -> Result<EncryptedMasterKey, IronOxideErr> {
        if bytes.len() == EncryptedMasterKey::SIZE_BYTES {
            let mut pbkdf2_salt = [0u8; PBKDF2_SALT_LEN];
            let mut aes_iv = [0u8; AES_IV_LEN];
            let mut encrypted_key = [0u8; ENCRYPTED_KEY_AND_GCM_TAG_LEN];
            pbkdf2_salt.copy_from_slice(&bytes[..PBKDF2_SALT_LEN]);
            aes_iv.copy_from_slice(&bytes[PBKDF2_SALT_LEN..(PBKDF2_SALT_LEN + AES_IV_LEN)]);
            encrypted_key.copy_from_slice(&bytes[(PBKDF2_SALT_LEN + AES_IV_LEN)..]);
            Ok(EncryptedMasterKey::new(pbkdf2_salt, aes_iv, encrypted_key))
        } else {
            Err(IronOxideErr::WrongSizeError(
                Some(bytes.len()),
                Some(EncryptedMasterKey::SIZE_BYTES),
            ))
        }
    }

    /// A bytes representation of EncryptedMasterKey
    /// The reciprocal of `EncryptedMasterKey::new_from_slice`
    pub fn bytes(&self) -> [u8; EncryptedMasterKey::SIZE_BYTES] {
        let mut dest = [0u8; EncryptedMasterKey::SIZE_BYTES];
        let vec = [
            &self.pbkdf2_salt[..],
            &self.aes_iv[..],
            &self.encrypted_key[..],
        ]
        .concat();

        debug_assert!(dest.len() == vec.len());

        dest.copy_from_slice(&vec[..]);
        dest
    }
}

pub struct AesEncryptedValue {
    aes_iv: [u8; AES_IV_LEN],
    ciphertext: Vec<u8>,
}
impl AesEncryptedValue {
    pub fn bytes(&self) -> Vec<u8> {
        [&self.aes_iv[..], &self.ciphertext].concat()
    }
}

impl TryFrom<&[u8]> for AesEncryptedValue {
    type Error = IronOxideErr;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        //AES encrypted values should be as long as the IV, GCM auth tag, and at least 1 encrypted byte
        if bytes.len() <= (AES_IV_LEN + AES_GCM_TAG_LEN + 1) {
            Err(IronOxideErr::AesEncryptedDocSizeError)
        } else {
            let mut iv: [u8; AES_IV_LEN] = [0u8; AES_IV_LEN];
            iv.copy_from_slice(&bytes[..AES_IV_LEN]);
            Ok(AesEncryptedValue {
                aes_iv: iv,
                ciphertext: bytes[AES_IV_LEN..].to_vec(),
            })
        }
    }
}

impl From<ring::error::Unspecified> for IronOxideErr {
    fn from(ring_err: Unspecified) -> Self {
        IronOxideErr::AesError(ring_err)
    }
}

/// Derive a key from a string password. Returns a tuple of salt that was used as part of the deriviation and the
/// key, both of which are 32 bytes.
fn derive_key_from_password(password: &str, salt: [u8; PBKDF2_SALT_LEN]) -> [u8; AES_KEY_LEN] {
    let mut derived_key = [0u8; digest::SHA256_OUTPUT_LEN];
    pbkdf2::derive(
        &digest::SHA256,
        PBKDF2_ITERATIONS,
        &salt,
        password.as_bytes(),
        &mut derived_key,
    );
    derived_key
}

/// Encrypt a users master private key using the provided password. Uses the password to generate a derived AES key
/// via PBKDF2 and then AES encrypts the users private key with the derived AES key.
pub fn encrypt_user_master_key<R: CryptoRng + RngCore>(
    rng: &mut R,
    password: &str,
    user_master_key: &[u8; 32],
) -> Result<EncryptedMasterKey, Unspecified> {
    let mut salt = [0u8; PBKDF2_SALT_LEN];
    rng.fill_bytes(&mut salt);
    let derived_key = derive_key_from_password(password, salt);

    let encrypted_key = encrypt(rng, &user_master_key.to_vec(), derived_key)?;
    //Convert the AES encrypted ciphertext vector into a fixed size array so that the
    //EncryptedMasterKey struct is all fixed size values
    let mut master_key_ciphertext = [0u8; ENCRYPTED_KEY_AND_GCM_TAG_LEN];
    master_key_ciphertext[..].copy_from_slice(&encrypted_key.ciphertext[..]);
    Ok(EncryptedMasterKey {
        pbkdf2_salt: salt,
        aes_iv: encrypted_key.aes_iv,
        encrypted_key: master_key_ciphertext,
    })
}

/// Decrypts a users encrypted master private key using the provided password. Uses the password and the provided pbkdf2 salt
/// to generate a derived AES key. Takes that derived AES key and uses it to try and decrypt the provided encrypted user master
/// key.
pub fn decrypt_user_master_key(
    password: &str,
    encrypted_master_key: &EncryptedMasterKey,
) -> Result<[u8; 32], Unspecified> {
    let derived_key = derive_key_from_password(password, encrypted_master_key.pbkdf2_salt);
    let mut fixed_decrypted_master_key = [0u8; 32];
    let mut encrypted_key = AesEncryptedValue {
        aes_iv: encrypted_master_key.aes_iv,
        ciphertext: encrypted_master_key.encrypted_key.to_vec(),
    };
    let decrypted_master_key = decrypt(&mut encrypted_key, derived_key)?;
    fixed_decrypted_master_key[..].copy_from_slice(decrypted_master_key);
    Ok(fixed_decrypted_master_key)
}

/// Encrypt the provided variable length plaintext with the provided 32 byte AES key. Returns a Result which
/// is a struct which contains the resulting ciphertext and the IV used during encryption.
pub fn encrypt<R: CryptoRng + RngCore>(
    rng: &mut R,
    plaintext: &Vec<u8>,
    key: [u8; AES_KEY_LEN],
) -> Result<AesEncryptedValue, Unspecified> {
    let algorithm = &aead::AES_256_GCM;

    let mut iv = [0u8; aead::NONCE_LEN];
    rng.fill_bytes(&mut iv);

    let aes_key = aead::SealingKey::new(algorithm, &key[..])?;

    //Increase the size of the plaintext vector to fit the GCM auth tag
    let mut ciphertext = plaintext.clone(); // <-- Not good. We're copying the entire plaintext, which could be large.
    ciphertext.resize(ciphertext.len() + algorithm.tag_len(), 0);
    aead::seal_in_place(
        &aes_key,
        aead::Nonce::assume_unique_for_key(iv),
        aead::Aad::empty(),
        &mut ciphertext,
        algorithm.tag_len(),
    )?;
    Ok(AesEncryptedValue {
        ciphertext,
        aes_iv: iv,
    })
}

use futures::future::IntoFuture;

/// Like `encrypt`, just wrapped in a Future for convenience
pub fn encrypt_future<R: CryptoRng + RngCore>(
    rng: &mut R,
    plaintext: &Vec<u8>,
    key: [u8; AES_KEY_LEN],
) -> impl Future<Item = AesEncryptedValue, Error = IronOxideErr> {
    encrypt(rng, plaintext, key)
        .map_err(IronOxideErr::from)
        .into_future()
}

/// Decrypt the provided ciphertext using the provided 12 byte IV and 32 byte key. Mutates the provided ciphertext
/// to be the decrypted value but leaves the auth tag at the end unmodified. Returns a result which is the plaintext
/// as an array.
pub fn decrypt(
    encrypted_doc: &mut AesEncryptedValue,
    key: [u8; AES_KEY_LEN],
) -> Result<&mut [u8], Unspecified> {
    let aes_key = aead::OpeningKey::new(&aead::AES_256_GCM, &key[..])?;

    let plaintext = aead::open_in_place(
        &aes_key,
        aead::Nonce::assume_unique_for_key(encrypted_doc.aes_iv),
        aead::Aad::empty(),
        0,
        &mut encrypted_doc.ciphertext[..],
    )?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_user_master_key() {
        let user_master_key = [0u8; 32];
        let password = "MyPassword";
        let mut rng = rand::thread_rng();
        let encrypted_master_key =
            encrypt_user_master_key(&mut rng, &password, &user_master_key).unwrap();
        assert_eq!(encrypted_master_key.pbkdf2_salt.len(), 32);
        assert_eq!(encrypted_master_key.aes_iv.len(), 12);
        assert_eq!(encrypted_master_key.encrypted_key.len(), 48);
    }

    #[test]
    fn test_decrypt_user_master_key() {
        let user_master_key = [0u8; 32];
        let password = "MyPassword";
        let mut rng = rand::thread_rng();
        let encrypted_master_key =
            encrypt_user_master_key(&mut rng, &password, &user_master_key).unwrap();

        let decrypted_master_key =
            decrypt_user_master_key(&password, &encrypted_master_key).unwrap();
        assert_eq!(decrypted_master_key, user_master_key);
    }

    #[test]
    fn test_encrypt() {
        let plaintext = vec![1, 2, 3, 4, 5, 6, 7];
        let mut key = [0u8; 32];
        let mut rng = rand::thread_rng();
        rng.fill_bytes(&mut key);

        let res = encrypt(&mut rng, &plaintext, key).unwrap();
        assert_eq!(res.aes_iv.len(), 12);
        assert_eq!(
            res.ciphertext.len(),
            plaintext.len() + &aead::AES_256_GCM.tag_len()
        );
    }

    #[test]
    fn test_decrypt() {
        let plaintext = vec![1, 2, 3, 4, 5, 6, 7];
        let mut key = [0u8; 32];
        let mut rng = rand::thread_rng();
        rng.fill_bytes(&mut key);

        let mut encrypted_result = encrypt(&mut rng, &plaintext, key).unwrap();

        let decrypted_plaintext = decrypt(&mut encrypted_result, key).unwrap();

        assert_eq!(*decrypted_plaintext, plaintext[..]);
    }
}