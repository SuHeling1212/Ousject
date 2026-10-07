//! Persistent users and bearer sessions for hosted Ousject.
//!
//! Passwords are stored as Argon2id PHC verifiers. Session secrets are
//! returned once and only a SHA-256 digest is persisted.

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{
        Error as PasswordHashError, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
};
use oms_runtime::{AccessContext, CreateObject, InMemoryObjectManager, ObjectQuery, Transaction};
use oms_types::{
    CORE_SESSION_TYPE, CORE_USER_TYPE, LOCAL_SUBJECT, LOCAL_USER_NAME, ObjectId, OmsError,
    SYSTEM_SUBJECT, SubjectId, Value,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

const DEFAULT_SESSION_SECONDS: u64 = 24 * 60 * 60;
const SALT_BYTES: usize = 16;
const TOKEN_BYTES: usize = 32;

/// Calculates a SHA-256 digest for content integrity checks.
#[must_use]
pub fn sha256_digest(message: &[u8]) -> [u8; 32] {
    Sha256::digest(message).into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Oms(OmsError),
    InvalidName,
    EmptyPassword,
    DuplicateUser,
    InvalidCredentials,
    InvalidToken,
    ExpiredSession,
    UnsupportedCredentialFormat,
    CredentialProcessing,
    Entropy(String),
    Clock,
    CorruptRecord(&'static str),
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPassword => formatter.write_str("password cannot be empty"),
            Self::UnsupportedCredentialFormat => {
                formatter.write_str("unsupported credential format")
            }
            Self::CredentialProcessing => formatter.write_str("credential processing failed"),
            Self::CorruptRecord(_) => formatter.write_str("corrupt authentication record"),
            _ => write!(formatter, "{self:?}"),
        }
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
        OsRng
            .try_fill_bytes(output)
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
    argon2_params: Params,
    session_seconds: u64,
}

impl AuthService {
    #[must_use]
    pub fn new(manager: Arc<InMemoryObjectManager>) -> Self {
        Self {
            manager,
            entropy: Arc::new(HostEntropy),
            argon2_params: Params::default(),
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
            // Keep unit tests fast without exposing weak settings in the
            // production constructor or persisted application configuration.
            argon2_params: Params::new(1_024, 1, 1, Some(32))
                .expect("valid test Argon2 parameters"),
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
        if name == LOCAL_USER_NAME || name.trim() != name || name.is_empty() || name.contains('\0')
        {
            return Err(AuthError::InvalidName);
        }
        if password.is_empty() {
            return Err(AuthError::EmptyPassword);
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
            return Err(AuthError::EmptyPassword);
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
        let verifier = self.hash_password(password)?;
        let state = Value::Record(BTreeMap::from([
            ("name".to_owned(), Value::Text(name.to_owned())),
            ("subject".to_owned(), Value::Text(subject.to_string())),
            ("password_hash".to_owned(), Value::Text(verifier)),
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

    fn hash_password(&self, password: &str) -> Result<String, AuthError> {
        let mut salt_bytes = [0_u8; SALT_BYTES];
        self.entropy.fill(&mut salt_bytes)?;
        let salt =
            SaltString::encode_b64(&salt_bytes).map_err(|_| AuthError::CredentialProcessing)?;
        let argon2 = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            self.argon2_params.clone(),
        );
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| AuthError::CredentialProcessing)
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
        self.verify_password(password, &user.password_hash)?;
        let mut token = [0_u8; TOKEN_BYTES];
        self.entropy.fill(&mut token)?;
        let now = unix_seconds()?;
        let expires_at = now
            .checked_add(self.session_seconds)
            .ok_or(AuthError::Clock)?;
        let object = ObjectId::new();
        let state = session_value(user.subject, sha256_digest(&token), now, expires_at)
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

    fn verify_password(&self, password: &str, verifier: &str) -> Result<(), AuthError> {
        let parsed = PasswordHash::new(verifier)
            .map_err(|_| AuthError::CorruptRecord("invalid password verifier"))?;
        if parsed.algorithm.as_str() != "argon2id" {
            return Err(AuthError::UnsupportedCredentialFormat);
        }
        let argon2 = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            self.argon2_params.clone(),
        );
        match argon2.verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(()),
            Err(PasswordHashError::Password) => Err(AuthError::InvalidCredentials),
            Err(_) => Err(AuthError::CorruptRecord("invalid password verifier")),
        }
    }

    /// Resolves a bearer token to its `SubjectId`.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, absent, revoked, corrupt or expired
    /// sessions.
    pub fn authenticate(&self, token: &str) -> Result<SubjectId, AuthError> {
        let token = hex_decode::<TOKEN_BYTES>(token).ok_or(AuthError::InvalidToken)?;
        let wanted = sha256_digest(&token);
        let now = unix_seconds()?;
        for header in self.manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
        )? {
            let value = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), header.id)?;
            let session = parse_session(&value)?;
            if wanted.ct_eq(&session.token_hash).unwrap_u8() == 1 {
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
        let wanted = sha256_digest(&token);
        for header in self.manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
        )? {
            let value = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), header.id)?;
            if wanted.ct_eq(&parse_session(&value)?.token_hash).unwrap_u8() == 1 {
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
            return Err(AuthError::EmptyPassword);
        }
        let Some((object, value)) = self.find_user(name)? else {
            return Err(AuthError::InvalidCredentials);
        };
        let user = parse_user(&value)?;
        if caller != LOCAL_SUBJECT && caller != user.subject {
            return Err(AuthError::InvalidCredentials);
        }
        let verifier = self.hash_password(password)?;
        let replacement = Value::Record(BTreeMap::from([
            ("name".to_owned(), Value::Text(user.name)),
            ("subject".to_owned(), Value::Text(user.subject.to_string())),
            ("password_hash".to_owned(), Value::Text(verifier)),
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
    password_hash: String,
}

#[derive(Debug)]
struct SessionRecord {
    subject: SubjectId,
    token_hash: [u8; 32],
    expires_at: u64,
}

fn parse_user(value: &Value) -> Result<UserRecord, AuthError> {
    let fields = record(value)?;
    let password_hash = match fields.get("password_hash") {
        Some(Value::Text(password_hash)) => password_hash.clone(),
        Some(Value::Bytes(_)) | None
            if fields.contains_key("salt") || fields.contains_key("rounds") =>
        {
            return Err(AuthError::UnsupportedCredentialFormat);
        }
        Some(_) => return Err(AuthError::CorruptRecord("invalid password verifier field")),
        None => return Err(AuthError::CorruptRecord("missing password verifier")),
    };
    if fields.contains_key("salt") || fields.contains_key("rounds") {
        return Err(AuthError::UnsupportedCredentialFormat);
    }
    Ok(UserRecord {
        name: record_text(value, "name")?.to_owned(),
        subject: record_text(value, "subject")?
            .parse()
            .map_err(|_| AuthError::CorruptRecord("invalid User subject"))?,
        password_hash,
    })
}

fn parse_session(value: &Value) -> Result<SessionRecord, AuthError> {
    let token_hash = record_bytes(value, "token_hash")?
        .try_into()
        .map_err(|_| AuthError::CorruptRecord("invalid Session token hash"))?;
    Ok(SessionRecord {
        subject: record_text(value, "subject")?
            .parse()
            .map_err(|_| AuthError::CorruptRecord("invalid Session subject"))?,
        token_hash,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct FixedEntropy(AtomicUsize);

    impl EntropyProvider for FixedEntropy {
        fn fill(&self, output: &mut [u8]) -> Result<(), AuthError> {
            let call = self.0.fetch_add(1, Ordering::Relaxed).to_le_bytes()[0];
            for (index, byte) in output.iter_mut().enumerate() {
                *byte = call
                    .wrapping_mul(31)
                    .wrapping_add(u8::try_from(index).unwrap_or(0))
                    .wrapping_add(1);
            }
            Ok(())
        }
    }

    fn auth(manager: Arc<InMemoryObjectManager>) -> AuthService {
        AuthService::with_test_settings(manager, Arc::new(FixedEntropy::default()))
    }

    fn stored_user_verifier(manager: &InMemoryObjectManager, user: &UserIdentity) -> String {
        let value = manager
            .value(AccessContext::new(SYSTEM_SUBJECT), user.object)
            .unwrap();
        record_text(&value, "password_hash").unwrap().to_owned()
    }

    #[test]
    fn user_login_session_and_logout_are_persistent_objects() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = auth(manager.clone());
        let user = auth.create_user("ada", "correct horse").unwrap();
        assert_eq!(auth.users().unwrap(), vec![user.clone()]);

        let verifier = stored_user_verifier(&manager, &user);
        assert!(verifier.starts_with("$argon2id$v=19$"));
        let Value::Record(user_fields) = manager
            .value(AccessContext::new(SYSTEM_SUBJECT), user.object)
            .unwrap()
        else {
            panic!("User state must be a record");
        };
        assert!(!user_fields.contains_key("salt"));
        assert!(!user_fields.contains_key("rounds"));
        assert_eq!(
            auth.login("ada", "wrong"),
            Err(AuthError::InvalidCredentials)
        );

        let session = auth.login("ada", "correct horse").unwrap();
        assert_eq!(session.subject, user.subject);
        assert_eq!(auth.authenticate(&session.token), Ok(user.subject));
        let session_value = manager
            .value(AccessContext::new(SYSTEM_SUBJECT), session.object)
            .unwrap();
        let token_bytes = hex_decode::<TOKEN_BYTES>(&session.token).unwrap();
        assert_eq!(
            record_bytes(&session_value, "token_hash").unwrap(),
            sha256_digest(&token_bytes)
        );
        assert!(!record(&session_value).unwrap().contains_key("token"));
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
    fn same_passwords_get_distinct_argon2id_phc_verifiers() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = auth(manager.clone());
        let first = auth.create_user("first", "same password").unwrap();
        let second = auth.create_user("second", "same password").unwrap();

        let first_verifier = stored_user_verifier(&manager, &first);
        let second_verifier = stored_user_verifier(&manager, &second);
        assert_ne!(first_verifier, second_verifier);
        assert!(first_verifier.starts_with("$argon2id$v=19$"));
        assert!(second_verifier.starts_with("$argon2id$v=19$"));
        assert_eq!(
            auth.login("first", "same password").unwrap().subject,
            first.subject
        );
        assert_eq!(
            auth.login("second", "same password").unwrap().subject,
            second.subject
        );
    }

    #[test]
    fn changing_password_replaces_phc_verifier() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = auth(manager.clone());
        let user = auth.create_user("ada", "old password").unwrap();
        let before = stored_user_verifier(&manager, &user);

        auth.change_password(user.subject, "ada", "new password")
            .unwrap();

        let after = stored_user_verifier(&manager, &user);
        assert_ne!(before, after);
        assert_eq!(
            auth.login("ada", "old password"),
            Err(AuthError::InvalidCredentials)
        );
        assert_eq!(
            auth.login("ada", "new password").unwrap().subject,
            user.subject
        );
    }

    #[test]
    fn argon2id_credentials_survive_store_reopen() {
        let directory = std::env::temp_dir().join(format!("ousject-auth-{}", ObjectId::new()));
        let path = directory.join("state.oms");
        {
            let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
            auth(manager.clone())
                .create_user("ada", "durable password")
                .unwrap();
        }
        {
            let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
            let session = auth(manager).login("ada", "durable password").unwrap();
            assert_eq!(
                AuthService::new(Arc::new(
                    InMemoryObjectManager::open_persistent(&path).unwrap()
                ))
                .authenticate(&session.token),
                Ok(session.subject)
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_pbkdf2_record_is_rejected_without_compatibility_fallback() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = auth(manager.clone());
        let user = auth.create_user("ada", "password").unwrap();
        let legacy_state = Value::Record(BTreeMap::from([
            ("name".to_owned(), Value::Text(user.name)),
            ("subject".to_owned(), Value::Text(user.subject.to_string())),
            ("salt".to_owned(), Value::Bytes(vec![1; SALT_BYTES])),
            ("password_hash".to_owned(), Value::Bytes(vec![2; 32])),
            ("rounds".to_owned(), Value::Integer(100_000)),
        ]))
        .encode()
        .unwrap();
        let header = manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), user.object)
            .unwrap();
        let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        transaction
            .expect(user.object, header.version)
            .update_state(user.object, legacy_state);
        manager.commit(transaction).unwrap();

        assert_eq!(
            auth.login("ada", "password"),
            Err(AuthError::UnsupportedCredentialFormat)
        );
    }

    #[test]
    fn malformed_phc_verifier_is_reported_without_panicking_or_leaking_it() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = auth(manager.clone());
        let user = auth.create_user("ada", "password").unwrap();
        let mut value = manager
            .value(AccessContext::new(SYSTEM_SUBJECT), user.object)
            .unwrap();
        let Value::Record(ref mut fields) = value else {
            unreachable!();
        };
        fields.insert(
            "password_hash".to_owned(),
            Value::Text("malformed private verifier".to_owned()),
        );
        let header = manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), user.object)
            .unwrap();
        let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        transaction
            .expect(user.object, header.version)
            .update_state(user.object, value.encode().unwrap());
        manager.commit(transaction).unwrap();

        let error = auth.login("ada", "password").unwrap_err();
        assert_eq!(error, AuthError::CorruptRecord("invalid password verifier"));
        assert_eq!(error.to_string(), "corrupt authentication record");
    }

    #[test]
    fn local_is_the_unique_named_highest_system_user() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let auth = auth(manager);
        assert_eq!(auth.initialize_local(""), Err(AuthError::EmptyPassword));
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
