mod account;
pub mod config;

use account::{AccountManagerError, Permissions};
use serde::Deserialize;
use std::ops::Deref;

#[async_std::main]
async fn main() -> tide::Result<()> {
    let mut app = tide::new();
    tide::log::with_level(tide::log::LevelFilter::Debug);
    account::INSTANCE.refresh_all().await;

    app.at("/api/account/create")
        .post(account::handle::create_account);
    app.at("/api/account/verify")
        .post(account::handle::verify_account);
    app.at("/api/account/login")
        .post(account::handle::login_account);
    app.at("/api/account/logout")
        .post(account::handle::logout_account);
    app.at("/api/account/signout")
        .post(account::handle::sign_out_account);
    app.at("/api/account/edit")
        .post(account::handle::edit_account);

    app.at("/api/account/manage/create")
        .post(account::handle::manage::make_account);
    app.at("/api/account/manage/view")
        .post(account::handle::manage::view_account);
    app.at("/api/account/manage/modify")
        .post(account::handle::manage::modify_account);

    app.listen("127.0.0.1:8080").await?;
    Ok(())
}

/// A context for checking the validation of action an account performs with permission requirements.
#[derive(Deserialize)]
pub struct RequirePermissionContext {
    /// The access token of this account.
    pub token: u64,
    /// The only id of this account.
    pub user_id: u64,
}

impl RequirePermissionContext {
    /// Indicates whether this context's token and permissions is valid.
    pub async fn valid(&self, permissions: Permissions) -> Result<bool, AccountManagerError> {
        let account_manager = account::INSTANCE.deref();
        match account_manager.index().read().await.get(&self.user_id) {
            Some(index) => {
                let b = account_manager.inner().read().await;
                let account = b.get(*index).unwrap().read().await;
                Ok(match account.deref() {
                    account::Account::Unverified(_) => {
                        return Err(AccountManagerError::Account(
                            self.user_id,
                            account::AccountError::UserUnverifiedError,
                        ))
                    }
                    account::Account::Verified { tokens, .. } => tokens.token_usable(self.token),
                } && permissions.iter().all(|p| account.has_permission(*p)))
            }
            None => Err(AccountManagerError::AccountNotFound(self.user_id)),
        }
    }
}
