pub mod handle;
pub mod verify;

use async_std::sync::RwLock;
use chrono::{DateTime, Duration, Utc};
use once_cell::sync::Lazy;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha256::digest;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    fmt::Display,
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
};
use tide::log::error;

pub use sms3rs_shared::account::*;

/// The static instance of accounts.
pub static INSTANCE: Lazy<AccountManager> = Lazy::new(AccountManager::new);

#[derive(Debug, Serialize, Deserialize)]
pub enum AccountError {
    /// Verification code not match.
    VerificationCodeError,
    /// User has not been verified.
    UserUnverifiedError,
    /// User already registered.
    UserRegisteredError,
    /// The target password is not correct.
    PasswordIncorrectError,
    /// The target token is not correct.
    TokenIncorrectError,
    /// The email address's domain is not from PKUSchool.
    EmailDomainNotInSchoolError,
    /// Date out of range.
    DateOutOfRangeError,
    /// An SMTP error.
    MailSendError(String),
    /// Permission denied.
    PermissionDeniedError,
}

impl Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountError::VerificationCodeError => f.write_str("Verification code not match"),
            AccountError::UserUnverifiedError => f.write_str("User has not been verified"),
            AccountError::UserRegisteredError => f.write_str("User already registered"),
            AccountError::PasswordIncorrectError => f.write_str("The password is not correct"),
            AccountError::TokenIncorrectError => f.write_str("The target token is not correct"),
            AccountError::EmailDomainNotInSchoolError => {
                f.write_str("The email address's domain is not from PKUSchool")
            }
            AccountError::DateOutOfRangeError => f.write_str("Date out of range"),
            AccountError::MailSendError(err) => {
                f.write_str("SMPT error while sending verification mail: ")?;
                f.write_str(err)
            }
            AccountError::PermissionDeniedError => f.write_str("Permission denied"),
        }
    }
}

impl std::error::Error for AccountError {}

/// Represent an account, including unverified and verified.
#[derive(Serialize, Deserialize, Debug)]
pub enum Account {
    /// An unverified account.
    Unverified(verify::Context),

    /// A normal user.
    Verified {
        /// Identifier of this user.
        id: u64,
        /// Attributes of this user.
        attributes: UserAttributes,
        /// This account's token manager.
        tokens: verify::Tokens,
        /// The verify context of this account exists in some conditions (ex. forget password).
        verify: UserVerifyVariant,
    },
}

impl Account {
    /// Create a new unverified account.
    pub async fn new(email: lettre::Address) -> Result<Self, AccountError> {
        if email.domain() != "i.pkuschool.edu.cn" && email.domain() != "pkuschool.edu.cn" {
            return Err(AccountError::EmailDomainNotInSchoolError);
        }
        Ok(Self::Unverified({
            let cxt = verify::Context {
                email,
                code: {
                    let mut rng = rand::thread_rng();
                    rng.gen_range(100000..999999)
                },
                expire_time: match Utc::now()
                    .naive_utc()
                    .checked_add_signed(Duration::minutes(15))
                {
                    Some(e) => e,
                    _ => return Err(AccountError::DateOutOfRangeError),
                },
            };
            cxt.send_verify().await.map_err(|err| {
                error!("Error while sending verification email: {}", err);
                err
            })?;
            cxt
        }))
    }

    /// Verify this account based on the variant.
    fn verify(
        &mut self,
        verify_code: u32,
        variant: AccountVerifyVariant,
    ) -> Result<(), AccountError> {
        match variant {
            AccountVerifyVariant::Activate(attributes) => {
                if let Self::Unverified(cxt) = self {
                    if cxt.code != verify_code {
                        return Err(AccountError::VerificationCodeError);
                    }
                    *self = Self::Verified {
                        id: {
                            let mut hasher = DefaultHasher::new();
                            attributes.email.hash(&mut hasher);
                            hasher.finish()
                        },
                        attributes,
                        tokens: verify::Tokens::new(),
                        verify: UserVerifyVariant::None,
                    };
                    Ok(())
                } else {
                    Err(AccountError::UserRegisteredError)
                }
            }
            AccountVerifyVariant::ResetPassword(password) => {
                if let Self::Verified {
                    attributes, verify, ..
                } = self
                {
                    match verify {
                        UserVerifyVariant::None => Err(AccountError::PermissionDeniedError),
                        UserVerifyVariant::ForgetPassword(cxt) => {
                            if cxt.code != verify_code {
                                return Err(AccountError::VerificationCodeError);
                            }
                            attributes.password_sha = digest(password);
                            *verify = UserVerifyVariant::None;
                            Ok(())
                        }
                    }
                } else {
                    Err(AccountError::UserUnverifiedError)
                }
            }
        }
    }

    /// Get the only id of this user.
    pub fn id(&self) -> u64 {
        match self {
            Account::Unverified(cxt) => {
                let mut hasher = DefaultHasher::new();
                cxt.email.hash(&mut hasher);
                hasher.finish()
            }
            Account::Verified { id, .. } => *id,
        }
    }

    /// Get email of this user.
    pub fn email(&self) -> &lettre::Address {
        match self {
            Account::Unverified(cxt) => &cxt.email,
            Account::Verified { attributes, .. } => &attributes.email,
        }
    }

    /// Get metadata of this user.
    pub fn metadata(&self) -> Result<UserMetadata, AccountError> {
        if let Self::Verified { attributes, .. } = self {
            Ok(UserMetadata {
                email: attributes.email.clone(),
                name: attributes.name.clone(),
                school_id: attributes.school_id,
                phone: attributes.phone,
                house: attributes.house,
                organization: attributes.organization.clone(),
            })
        } else {
            Err(AccountError::UserUnverifiedError)
        }
    }

    /// Get all permissions this user has.
    pub fn permissions(&self) -> Permissions {
        match self {
            Account::Unverified(_) => Vec::new(),
            Account::Verified { attributes, .. } => attributes.permissions.clone(),
        }
    }

    /// Indicates whether this user has the target permission.
    pub fn has_permission(&self, permission: Permission) -> bool {
        self.permissions().contains(&permission)
    }

    /// Login into the account and return back a token in a `Result`.
    pub fn login(&mut self, password: &str) -> Result<String, AccountError> {
        match self {
            Account::Unverified(_) => Err(AccountError::UserUnverifiedError),
            Account::Verified {
                id,
                attributes,
                tokens,
                ..
            } => {
                if digest(password) == attributes.password_sha {
                    Ok(tokens.new_token(*id, attributes.token_expiration_time))
                } else {
                    Err(AccountError::PasswordIncorrectError)
                }
            }
        }
    }

    /// Logout this account with the target token.
    pub fn logout(&mut self, token: &str) -> Result<(), AccountError> {
        match self {
            Account::Unverified(_) => Err(AccountError::UserUnverifiedError),
            Account::Verified { tokens, .. } => {
                if tokens.remove(token) {
                    Ok(())
                } else {
                    Err(AccountError::TokenIncorrectError)
                }
            }
        }
    }

    /// Save this account and return whether if this account was saved successfully.
    #[cfg(not(test))]
    #[must_use = "The save result should be handled"]
    pub fn save(&self) -> bool {
        use std::fs::File;
        use std::io::Write;
        if let Ok(mut file) = File::create(format!("./data/accounts/{}.toml", self.id())) {
            file.write_all(
                match toml::to_string(&self) {
                    Ok(e) => e,
                    _ => return false,
                }
                .as_bytes(),
            )
            .is_ok()
        } else {
            false
        }
    }

    /// Save this account and return whether if this account was saved successfully.
    #[cfg(test)]
    #[must_use = "The save result should be handled"]
    pub fn save(&self) -> bool {
        true
    }

    /// Remove this account from filesystem and return whether this account was removed successfully.
    #[cfg(not(test))]
    pub fn remove(&self) -> bool {
        use std::fs;
        fs::remove_file(format!("./data/accounts/{}.json", self.id())).is_ok()
    }

    /// Remove this account from filesystem and return whether this account was removed successfully.
    #[cfg(test)]
    pub fn remove(&self) -> bool {
        true
    }
}

enum AccountVerifyVariant {
    /// Activate an unverified account.
    Activate(UserAttributes),
    /// Reset a forgotten password.
    ResetPassword(String),
}

#[derive(Deserialize, Serialize, Debug)]
pub enum UserVerifyVariant {
    None,
    ForgetPassword(verify::Context),
}

// Attributes of a registered user.
#[derive(Serialize, Deserialize, Debug)]
pub struct UserAttributes {
    /// Email address of this user.
    pub email: lettre::Address,
    /// Name of this user.
    pub name: String,
    /// School id of this user (ex. 2522xxx).
    pub school_id: u32,
    /// Phone number of this user.
    pub phone: u64,
    /// House this student belongs to. Can be `None`.
    pub house: Option<House>,
    /// Organization this user belongs to. Can be `None`.
    pub organization: Option<String>,
    /// Permissions this user has.
    pub permissions: Permissions,
    /// The registration time of this user.
    pub registration_time: DateTime<Utc>,
    /// The registration ip of this user.
    pub registration_ip: Option<String>,
    /// Hash of this user's password.
    pub password_sha: String,
    /// The expiration time of a token in days.
    /// `0` means never expire.
    pub token_expiration_time: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum AccountManagerError {
    Account(u64, AccountError),
    AccountNotFound(u64),
}

impl Display for AccountManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountManagerError::Account(id, e) => {
                f.write_str("Account ")?;
                id.fmt(f)?;
                f.write_str(" Errored: ")?;
                e.fmt(f)
            }
            AccountManagerError::AccountNotFound(id) => {
                f.write_str("Account ")?;
                id.fmt(f)?;
                f.write_str(" not found")
            }
        }
    }
}

impl std::error::Error for AccountManagerError {}

/// A simple account manager.
pub struct AccountManager {
    accounts: RwLock<Vec<RwLock<Account>>>,
    /// An index cache for getting index from an id.
    index: RwLock<HashMap<u64, usize>>,
}

impl AccountManager {
    /// Read and create an account manager from `./data/accounts`.
    pub fn new() -> Self {
        #[cfg(not(test))]
        {
            use std::fs::{self, File};
            use std::io::Read;

            let mut vec = Vec::new();
            let mut index = HashMap::new();
            let mut i = 0;
            for dir in fs::read_dir("./data/accounts").unwrap() {
                if let Ok(e) = dir.map(|e| {
                    toml::from_str::<Account>(&{
                        let mut string = String::new();
                        File::open(e.path())
                            .unwrap()
                            .read_to_string(&mut string)
                            .unwrap();
                        string
                    })
                    .unwrap()
                }) {
                    index.insert(e.id(), i);
                    vec.push(RwLock::new(e));
                    i += 1;
                } else {
                    continue;
                }
            }
            Self {
                accounts: RwLock::new(vec),
                index: RwLock::new(index),
            }
        }

        #[cfg(test)]
        Self {
            accounts: RwLock::new(Vec::new()),
            index: RwLock::new(HashMap::new()),
        }
    }

    /// Get inner accounts.
    pub fn inner(&self) -> &RwLock<Vec<RwLock<Account>>> {
        &self.accounts
    }

    /// Get inner indexe cache.
    pub fn index(&self) -> &RwLock<HashMap<u64, usize>> {
        &self.index
    }

    /// Update index cache of this instance.
    pub async fn update_index(&self) {
        let mut map = HashMap::new();
        for account in self.accounts.read().await.iter().enumerate() {
            map.insert(account.1.read().await.id(), account.0);
        }
        let mut iw = self.index.write().await;
        *iw.deref_mut() = map;
    }

    /// Refresh this instance.
    ///
    /// - Remove expired unverified accounts
    /// - Remove expired tokens
    pub async fn refresh_all(&self) {
        {
            let mut rm_list: Vec<usize> = Vec::new();
            for account in self.accounts.read().await.iter().enumerate() {
                {
                    let r_binding = account.1.read().await;
                    if match r_binding.deref() {
                        Account::Unverified(cxt) => cxt.is_expired(),
                        _ => false,
                    } {
                        rm_list.push(account.0);
                    }
                }
            }
            let mut w = self.accounts.write().await;
            for i in rm_list.iter().enumerate() {
                w.remove(*i.1 - i.0);
            }
            if !rm_list.is_empty() {
                self.update_index().await;
            }
        }
        {
            for account in self.accounts.read().await.iter() {
                let mut w = account.write().await;
                if let Account::Verified { tokens, verify, .. } = w.deref_mut() {
                    tokens.refresh();
                    if match verify {
                        UserVerifyVariant::None => false,
                        UserVerifyVariant::ForgetPassword(e) => e.is_expired(),
                    } {
                        *verify = UserVerifyVariant::None;
                    }
                }
            }
        }
    }

    /// Refresh target account.
    ///
    /// - Remove expired unverified account;
    /// - Remove expired tokens.
    pub async fn refresh(&self, id: u64) {
        if let Some(index) = self.index.read().await.get(&id) {
            if let Some(account) = self.accounts.read().await.get(*index) {
                {
                    if match account.read().await.deref() {
                        Account::Unverified(cxt) => cxt.is_expired(),
                        _ => false,
                    } {
                        self.remove(id).await;
                    }
                }
                {
                    match account.write().await.deref_mut() {
                        Account::Verified { tokens, verify, .. } => {
                            tokens.refresh();
                            if match verify {
                                UserVerifyVariant::None => false,
                                UserVerifyVariant::ForgetPassword(e) => e.is_expired(),
                            } {
                                *verify = UserVerifyVariant::None;
                            }
                        }
                        _ => (),
                    }
                }
            }
        }
    }

    /// Remove target account.
    pub async fn remove(&self, id: u64) {
        if let Some(index) = self.index.read().await.get(&id) {
            {
                let b = self.accounts.read().await;
                b.get(*index).unwrap().read().await.remove();
            }
            self.accounts.write().await.remove(*index);
        }
        self.update_index().await;
    }

    /// Push an account to this instance, only for testing.
    #[cfg(test)]
    pub async fn push(&self, account: Account) {
        assert!(self
            .index
            .write()
            .await
            .insert(account.id(), self.accounts.read().await.len())
            .is_none());
        self.accounts.write().await.push(RwLock::new(account));
    }

    #[cfg(test)]
    pub async fn reset(&self) {
        *self.accounts.write().await.deref_mut() = Vec::new();
        *self.index.write().await.deref_mut() = HashMap::new();
    }
}
