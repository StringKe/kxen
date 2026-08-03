//! OAuth refresh 与 run cancel 的统一竞态边界。

use std::future::Future;

use crate::agent::cancel::CancelToken;
use crate::auth::credential::AuthStore;
use crate::auth::refresh::RefreshOutcome;
use crate::llm::ModelRef;

pub(super) async fn ensure(store: &mut AuthStore, model: &ModelRef, cancel: Option<&CancelToken>) -> Result<RefreshOutcome, ()> {
    cancellable(crate::auth::refresh::ensure_fresh(store, &model.provider, model.account.as_deref()), cancel).await
}

pub(super) async fn force(store: &mut AuthStore, model: &ModelRef, cancel: Option<&CancelToken>) -> Result<RefreshOutcome, ()> {
    cancellable(crate::auth::refresh::force_refresh(store, &model.provider, model.account.as_deref()), cancel).await
}

async fn cancellable<F, T>(future: F, cancel: Option<&CancelToken>) -> Result<T, ()>
where
    F: Future<Output = T>,
{
    match cancel {
        Some(token) => tokio::select! {
            value = future => Ok(value),
            _ = token.wait() => Err(()),
        },
        None => Ok(future.await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_interrupts_a_pending_refresh() {
        let cancel = CancelToken::new();
        let waiting = cancellable(std::future::pending::<()>(), Some(&cancel));
        let trigger = async {
            tokio::task::yield_now().await;
            cancel.cancel();
        };

        let (result, ()) = tokio::join!(waiting, trigger);

        assert!(result.is_err());
    }
}
