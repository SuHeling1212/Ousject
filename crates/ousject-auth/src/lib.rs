//! Persistent users and bearer sessions for hosted Ousject.
//!
//! Passwords are stored as salted PBKDF2-HMAC-SHA256 verifiers. Session
//! secrets are returned once and only a SHA-256 digest is persisted.

use oms_runtime::{AccessContext, CreateObject, InMemoryObjectManager, ObjectQuery, Transaction};
use oms_types::{
    CORE_SESSION_TYPE, CORE_USER_TYPE, LOCAL_SUBJECT, LOCAL_USER_NAME, ObjectId, OmsError,
    SYSTEM_SUBJECT, SubjectId, Value,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_KDF_ROUNDS: u32 = 100_000;
const DEFAULT_SESSION_SECONDS: u64 = 24 * 60 * 60;
const SALT_BYTES: usize = 16;
const TOKEN_BYTES: usize = 32;

/// Calculates a SHA-256 digest for content integrity checks.
#[must_use]
pub fn sha256_digest(message: &[u8]) -> [u8; 32] {
    sha256(message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Oms(OmsError),
    InvalidName,
    DuplicateUser,
    InvalidCredentials,
    InvalidToken,
    ExpiredSession,
    Entropy(String),
    Clock,
    CorruptRecord(&'static str),
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for AuthError {}

impl From<OmsError> for AuthError {
    fn from(error: OmsError) -> Self {
        Self::Oms(error)
    }
}

pub trait EntropyProvider: fmt::Debug + Send + Sync {
    /// Fills every byte with unpredictable entropy.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when the hardware/host entropy source fails.
    fn fill(&self, output: &mut [u8]) -> Result<(), AuthError>;
}

#[derive(Debug)]
pub struct HostEntropy;

impl EntropyProvider for HostEntropy {
    fn fill(&self, output: &mut [u8]) -> Result<(), AuthError> {
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(output))
            .map_err(|error| AuthError::Entropy(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIdentity {
    pub object: ObjectId,
    pub subject: SubjectId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSecret {
    pub object: ObjectId,
    pub subject: SubjectId,
    pub token: String,
    pub expires_at: u64,
}

#[derive(Debug)]
pub struct AuthService {
    manager: Arc<InMemoryObjectManager>,
    entropy: Arc<dyn EntropyProvider>,
    kdf_rounds: u32,
    session_seconds: u64,
}

impl AuthService {
    #[must_use]
    pub fn new(manager: Arc<InMemoryObjectManager>) -> Self {
        Self {
            manager,
            entropy: Arc::new(HostEntropy),
            kdf_rounds: DEFAULT_KDF_ROUNDS,
            session_seconds: DEFAULT_SESSION_SECONDS,
        }
    }

    #[cfg(test)]
    fn with_test_settings(
        manager: Arc<InMemoryObjectManager>,
        entropy: Arc<dyn EntropyProvider>,
    ) -> Self {
        Self {
            manager,
            entropy,
            kdf_rounds: 8,
            session_seconds: DEFAULT_SESSION_SECONDS,
        }
    }

    /// Creates one persistent User Object with a salted password verifier.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid/duplicate name, weak empty password,
    /// entropy failure, or failed atomic persistence.
    pub fn create_user(&self, name: &str, password: &str) -> Result<UserIdentity, AuthError> {
        let mut transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        let identity = self.stage_create_user(name, password, &mut transaction)?;
        self.manager.commit(transaction)?;
        Ok(identity)
    }

    /// Stages User creation in a caller-owned transaction.
    ///
    /// # Errors
    ///
    /// Returns the same validation, entropy, and lookup errors as `create_user`.
    pub fn stage_create_user(
        &self,
        name: &str,
        password: &str,
        transaction: &mut Transaction,
    ) -> Result<UserIdentity, AuthError> {
        if name == LOCAL_USER_NAME
            || name.trim() != name
            || name.is_empty()
            || name.contains('\0')
            || password.is_empty()
        {
            return Err(AuthError::InvalidName);
        }
        if self.find_user(name)?.is_some() {
            return Err(AuthError::DuplicateUser);
        }
        let object = ObjectId::new();
        self.stage_persist_user(
            object,
            SubjectId::from_u128(object.as_u128()),
            name,
            password,
            transaction,
        )
    }

    /// Creates the one reserved highest-privilege `local` User identity.
    ///
    /// # Errors
    ///
    /// Returns an error if `local` already exists, its reserved Subject is in
    /// use, the password is empty, entropy fails, or persistence fails.
    pub fn initialize_local(&self, password: &str) -> Result<UserIdentity, AuthError> {
        let mut transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        let identity = self.stage_initialize_local(password, &mut transaction)?;
        self.manager.commit(transaction)?;
        Ok(identity)
    }

    /// Stages creation of the reserved `local` identity.
    ///
    /// # Errors
    ///
    /// Returns the same validation, entropy, and lookup errors as
    /// `initialize_local`.
    pub fn stage_initialize_local(
        &self,
        password: &str,
        transaction: &mut Transaction,
    ) -> Result<UserIdentity, AuthError> {
        if password.is_empty() {
            return Err(AuthError::InvalidName);
        }
        if self.find_user(LOCAL_USER_NAME)?.is_some()
            || self
                .users()?
                .iter()
                .any(|identity| identity.subject == LOCAL_SUBJECT)
        {
            return Err(AuthError::DuplicateUser);
        }
        self.stage_persist_user(
            ObjectId::new(),
            LOCAL_SUBJECT,
            LOCAL_USER_NAME,
            password,
            transaction,
        )
    }

    fn stage_persist_user(
        &self,
        object: ObjectId,
        subject: SubjectId,
        name: &str,
        password: &str,
        transaction: &mut Transaction,
    ) -> Result<UserIdentity, AuthError> {
        let mut salt = [0_u8; SALT_BYTES];
        self.entropy.fill(&mut salt)?;
        let verifier = pbkdf2_sha256(password.as_bytes(), &salt, self.kdf_rounds);
        let rounds = i64::from(self.kdf_rounds);
        let state = Value::Record(BTreeMap::from([
            ("name".to_owned(), Value::Text(name.to_owned())),
            ("subject".to_owned(), Value::Text(subject.to_string())),
            ("salt".to_owned(), Value::Bytes(salt.to_vec())),
            ("password_hash".to_owned(), Value::Bytes(verifier.to_vec())),
            ("rounds".to_owned(), Value::Integer(rounds)),
        ]))
        .encode()
        .map_err(|_| AuthError::CorruptRecord("cannot encode User"))?;
        transaction.create(
            CreateObject::new(CORE_USER_TYPE, state)
                .with_id(object)
                .with_grant(subject, oms_types::Capability::Inspect)
                .with_grant(subject, oms_types::Capability::ViewValue),
        );
        Ok(UserIdentity {
            object,
            subject,
            name: name.to_owned(),
        })
    }

    /// Verifies a password and creates a persistent expiring Session Object.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCredentials` without revealing whether a user exists.
    pub fn login(&self, name: &str, password: &str) -> Result<SessionSecret, AuthError> {
        let mut transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        let session = self.stage_login(name, password, &mut transaction)?;
        self.manager.commit(transaction)?;
        Ok(session)
    }

    /// Stages Session creation so login and Process identity transition can
    /// share one commit.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid credentials, entropy/clock failure, or a
    /// corrupt persistent User.
    pub fn stage_login(
        &self,
        name: &str,
        password: &str,
        transaction: &mut Transaction,
    ) -> Result<SessionSecret, AuthError> {
        let Some((_, record)) = self.find_user(name)? else {
            return Err(AuthError::InvalidCredentials);
        };
        let user = parse_user(&record)?;
        let actual = pbkdf2_sha256(password.as_bytes(), &user.salt, user.rounds);
        if !constant_time_eq(&actual, &user.password_hash) {
            return Err(AuthError::InvalidCredentials);
        }
        let mut token = [0_u8; TOKEN_BYTES];
        self.entropy.fill(&mut token)?;
        let now = unix_seconds()?;
        let expires_at = now
            .checked_add(self.session_seconds)
            .ok_or(AuthError::Clock)?;
        let object = ObjectId::new();
        let state = session_value(user.subject, sha256(&token), now, expires_at)
            .encode()
            .map_err(|_| AuthError::CorruptRecord("cannot encode Session"))?;
        transaction.create(
            CreateObject::new(CORE_SESSION_TYPE, state)
                .with_id(object)
                .with_grant(user.subject, oms_types::Capability::Inspect)
                .with_grant(user.subject, oms_types::Capability::ViewValue)
                .with_grant(user.subject, oms_types::Capability::Invoke)
                .with_grant(user.subject, oms_types::Capability::Retire),
        );
        Ok(SessionSecret {
            object,
            subject: user.subject,
            token: hex_encode(&token),
            expires_at,
        })
    }

    /// Resolves a bearer token to its `SubjectId`.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, absent, revoked, corrupt or expired
    /// sessions.
    pub fn authenticate(&self, token: &str) -> Result<SubjectId, AuthError> {
        let token = hex_decode::<TOKEN_BYTES>(token).ok_or(AuthError::InvalidToken)?;
        let wanted = sha256(&token);
        let now = unix_seconds()?;
        for header in self.manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
        )? {
            let value = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), header.id)?;
            let session = parse_session(&value)?;
            if constant_time_eq(&wanted, &session.token_hash) {
                return if now >= session.expires_at {
                    Err(AuthError::ExpiredSession)
                } else {
                    Ok(session.subject)
                };
            }
        }
        Err(AuthError::InvalidToken)
    }

    /// Revokes a session by retiring its persistent Session Object.
    ///
    /// # Errors
    ///
    /// Returns an error when the token is invalid or the retirement fails.
    pub fn logout(&self, token: &str) -> Result<(), AuthError> {
        let mut transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        self.stage_logout(token, &mut transaction)?;
        self.manager.commit(transaction)?;
        Ok(())
    }

    /// Stages Session revocation in a caller-owned transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid token or corrupt persistent Session.
    pub fn stage_logout(
        &self,
        token: &str,
        transaction: &mut Transaction,
    ) -> Result<(), AuthError> {
        let token = hex_decode::<TOKEN_BYTES>(token).ok_or(AuthError::InvalidToken)?;
        let wanted = sha256(&token);
        for header in self.manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
        )? {
            let value = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), header.id)?;
            if constant_time_eq(&wanted, &parse_session(&value)?.token_hash) {
                transaction
                    .expect(header.id, header.version)
                    .tombstone(header.id);
                return Ok(());
            }
        }
        Err(AuthError::InvalidToken)
    }

    /// Lists public User identities without credential verifier fields.
    ///
    /// # Errors
    ///
    /// Returns an error when User Objects cannot be read or are corrupt.
    pub fn users(&self) -> Result<Vec<UserIdentity>, AuthError> {
        let mut users = Vec::new();
        for header in self.manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_USER_TYPE),
        )? {
            let value = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), header.id)?;
            let user = parse_user(&value)?;
            users.push(UserIdentity {
                object: header.id,
                subject: user.subject,
                name: user.name,
            });
        }
        Ok(users)
    }

    /// Replaces a User password verifier. A User may change its own password;
    /// `local` may change any User password.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown User, an unauthorized caller, an empty
    /// password, entropy failure, or failed atomic persistence.
    pub fn change_password(
        &self,
        caller: SubjectId,
        name: &str,
        password: &str,
    ) -> Result<(), AuthError> {
        let mut transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        self.stage_change_password(caller, name, password, &mut transaction)?;
        self.manager.commit(transaction)?;
        Ok(())
    }

    /// Stages a password verifier replacement in a caller-owned transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid authorization/input, entropy failure, or a
    /// corrupt persistent User.
    pub fn stage_change_password(
        &self,
        caller: SubjectId,
        name: &str,
        password: &str,
        transaction: &mut Transaction,
    ) -> Result<(), AuthError> {
        if password.is_empty() {
            return Err(AuthError::InvalidName);
        }
        let Some((object, value)) = self.find_user(name)? else {
            return Err(AuthError::InvalidCredentials);
        };
        let user = parse_user(&value)?;
        if caller != LOCAL_SUBJECT && caller != user.subject {
            return Err(AuthError::InvalidCredentials);
        }
        let mut salt = [0_u8; SALT_BYTES];
        self.entropy.fill(&mut salt)?;
        let replacement = Value::Record(BTreeMap::from([
            ("name".to_owned(), Value::Text(user.name)),
            ("subject".to_owned(), Value::Text(user.subject.to_string())),
            ("salt".to_owned(), Value::Bytes(salt.to_vec())),
            (
                "password_hash".to_owned(),
                Value::Bytes(pbkdf2_sha256(password.as_bytes(), &salt, self.kdf_rounds).to_vec()),
            ),
            (
                "rounds".to_owned(),
                Value::Integer(i64::from(self.kdf_rounds)),
            ),
        ]));
        let header = self
            .manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), object)?;
        transaction.expect(object, header.version).update_state(
            object,
            replacement
                .encode()
                .map_err(|_| AuthError::CorruptRecord("cannot encode User"))?,
        );
        Ok(())
    }

    /// Disables a non-`local` User and all of its active Sessions atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown/reserved User or failed persistence.
    pub fn disable_user(&self, name: &str) -> Result<(), AuthError> {
        let mut transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        self.stage_disable_user(name, &mut transaction)?;
        self.manager.commit(transaction)?;
        Ok(())
    }

    /// Stages User and Session retirement in a caller-owned transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown/reserved User or corrupt persistent
    /// identity state.
    pub fn stage_disable_user(
        &self,
        name: &str,
        transaction: &mut Transaction,
    ) -> Result<(), AuthError> {
        let Some((object, value)) = self.find_user(name)? else {
            return Err(AuthError::InvalidCredentials);
        };
        let user = parse_user(&value)?;
        if user.subject == LOCAL_SUBJECT {
            return Err(AuthError::InvalidName);
        }
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let header = self.manager.inspect(context, object)?;
        transaction.expect(object, header.version).tombstone(object);
        for session in self
            .manager
            .query(context, &ObjectQuery::new().with_type(CORE_SESSION_TYPE))?
        {
            if parse_session(&self.manager.value(context, session.id)?)?.subject == user.subject {
                transaction
                    .expect(session.id, session.version)
                    .tombstone(session.id);
            }
        }
        Ok(())
    }

    fn find_user(&self, name: &str) -> Result<Option<(ObjectId, Value)>, AuthError> {
        for header in self.manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_USER_TYPE),
        )? {
            let value = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), header.id)?;
            if record_text(&value, "name")? == name {
                return Ok(Some((header.id, value)));
            }
        }
        Ok(None)
    }
}

#[derive(Debug)]
struct UserRecord {
    name: String,
    subject: SubjectId,
    salt: Vec<u8>,
    password_hash: Vec<u8>,
    rounds: u32,
}

#[derive(Debug)]
struct SessionRecord {
    subject: SubjectId,
    token_hash: Vec<u8>,
    expires_at: u64,
}

fn parse_user(value: &Value) -> Result<UserRecord, AuthError> {
    let rounds = record_integer(value, "rounds")?;
    Ok(UserRecord {
        name: record_text(value, "name")?.to_owned(),
        subject: record_text(value, "subject")?
            .parse()
            .map_err(|_| AuthError::CorruptRecord("invalid User subject"))?,
        salt: record_bytes(value, "salt")?.to_vec(),
        password_hash: record_bytes(value, "password_hash")?.to_vec(),
        rounds: u32::try_from(rounds)
            .ok()
            .filter(|rounds| *rounds > 0)
            .ok_or(AuthError::CorruptRecord("invalid User KDF rounds"))?,
    })
}

fn parse_session(value: &Value) -> Result<SessionRecord, AuthError> {
    Ok(SessionRecord {
        subject: record_text(value, "subject")?
            .parse()
            .map_err(|_| AuthError::CorruptRecord("invalid Session subject"))?,
        token_hash: record_bytes(value, "token_hash")?.to_vec(),
        expires_at: u64::try_from(record_integer(value, "expires_at")?)
            .map_err(|_| AuthError::CorruptRecord("invalid Session expiry"))?,
    })
}

fn session_value(subject: SubjectId, token_hash: [u8; 32], now: u64, expiry: u64) -> Value {
    Value::Record(BTreeMap::from([
        ("subject".to_owned(), Value::Text(subject.to_string())),
        ("token_hash".to_owned(), Value::Bytes(token_hash.to_vec())),
        (
            "created_at".to_owned(),
            Value::Integer(i64::try_from(now).unwrap_or(i64::MAX)),
        ),
        (
            "expires_at".to_owned(),
            Value::Integer(i64::try_from(expiry).unwrap_or(i64::MAX)),
        ),
    ]))
}

fn record(value: &Value) -> Result<&BTreeMap<String, Value>, AuthError> {
    let Value::Record(fields) = value else {
        return Err(AuthError::CorruptRecord("expected Record"));
    };
    Ok(fields)
}

fn record_text<'a>(value: &'a Value, key: &str) -> Result<&'a str, AuthError> {
    let Some(Value::Text(value)) = record(value)?.get(key) else {
        return Err(AuthError::CorruptRecord("expected Text field"));
    };
    Ok(value)
}

fn record_bytes<'a>(value: &'a Value, key: &str) -> Result<&'a [u8], AuthError> {
    let Some(Value::Bytes(value)) = record(value)?.get(key) else {
        return Err(AuthError::CorruptRecord("expected Bytes field"));
    };
    Ok(value)
}

fn record_integer(value: &Value, key: &str) -> Result<i64, AuthError> {
    let Some(Value::Integer(value)) = record(value)?.get(key) else {
        return Err(AuthError::CorruptRecord("expected Integer field"));
    };
    Ok(*value)
}

fn unix_seconds() -> Result<u64, AuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| AuthError::Clock)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn hex_decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut output = [0_u8; N];
    for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(output)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut input = Vec::with_capacity(salt.len() + 4);
    input.extend_from_slice(salt);
    input.extend_from_slice(&1_u32.to_be_bytes());
    let mut current = hmac_sha256(password, &input);
    let mut result = current;
    for _ in 1..rounds {
        current = hmac_sha256(password, &current);
        for (target, value) in result.iter_mut().zip(current) {
            *target ^= value;
        }
    }
    result
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    inner.extend(block.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(message);
    let inner_hash = sha256(&inner);
    let mut outer = Vec::with_capacity(96);
    outer.extend(block.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    sha256(&outer)
}

#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let bit_length = u64::try_from(message.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());
    let mut hash = [
        0x6a09_e667_u32,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(word.try_into().expect("four-byte chunk"));
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (target, value) in hash.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *target = target.wrapping_add(value);
        }
    }
    let mut output = [0_u8; 32];
    for (chunk, word) in output.chunks_exact_mut(4).zip(hash) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixedEntropy;

    impl EntropyProvider for FixedEntropy {
        fn fill(&self, output: &mut [u8]) -> Result<(), AuthError> {
            for (index, byte) in output.iter_mut().enumerate() {
                *byte = u8::try_from(index).unwrap_or(0).wrapping_add(1);
            }
            Ok(())
        }
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            hex_encode(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn pbkdf2_matches_known_vector() {
        assert_eq!(
            hex_encode(&pbkdf2_sha256(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }

    #[test]
    fn user_login_session_and_logout_are_persistent_objects() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = AuthService::with_test_settings(manager.clone(), Arc::new(FixedEntropy));
        let user = auth.create_user("ada", "correct horse").unwrap();
        assert_eq!(auth.users().unwrap(), vec![user.clone()]);
        assert_eq!(
            auth.login("ada", "wrong"),
            Err(AuthError::InvalidCredentials)
        );
        let session = auth.login("ada", "correct horse").unwrap();
        assert_eq!(session.subject, user.subject);
        assert_eq!(auth.authenticate(&session.token), Ok(user.subject));
        auth.logout(&session.token).unwrap();
        assert_eq!(
            auth.authenticate(&session.token),
            Err(AuthError::InvalidToken)
        );
        assert!(
            manager
                .list(AccessContext::new(SYSTEM_SUBJECT))
                .unwrap()
                .iter()
                .any(|header| {
                    header.id == session.object
                        && header.lifecycle == oms_types::LifecycleState::Tombstoned
                })
        );
    }

    #[test]
    fn local_is_the_unique_named_highest_system_user() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = AuthService::with_test_settings(manager, Arc::new(FixedEntropy));
        let local = auth.initialize_local("local password").unwrap();

        assert_eq!(local.name, LOCAL_USER_NAME);
        assert_eq!(local.subject, LOCAL_SUBJECT);
        assert_eq!(
            auth.login(LOCAL_USER_NAME, "local password")
                .unwrap()
                .subject,
            LOCAL_SUBJECT
        );
        assert_eq!(
            auth.initialize_local("another password"),
            Err(AuthError::DuplicateUser)
        );
        assert_eq!(
            auth.create_user(LOCAL_USER_NAME, "password"),
            Err(AuthError::InvalidName)
        );
    }
}
