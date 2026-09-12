//! Insights jobs and ad-account discovery.
use super::{Client, REVIEW_POLL_INTERVAL};
use crate::error::Error;
use crate::insights::{AdAccountsReply, InsightsJob, InsightsQuery, InsightsReply};
#[cfg(feature = "meta-ads")]
use crate::insights::{InsightsJobStatus, InsightsJobWait};
use crate::types::{AccountKey, Capability, Deadline};

impl Client {
    /// Read metrics (026 read seam). Same orchestration as [`publish`](Client::publish)
    /// minus everything only a publication is entitled to: no idempotency
    /// (a read has no side effect to dedupe) and no ledger. The range is
    /// re-validated here even though the CLI checks it — the library cannot
    /// trust every caller.
    pub async fn insights(
        &self,
        key: &AccountKey,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let query = query.clone();
            Box::pin(async move { source.insights(&app, &creds, &query, deadline).await })
        })
        .await
    }

    pub async fn start_insights_job(
        &self,
        key: &AccountKey,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let query = query.clone();
            Box::pin(async move {
                source
                    .start_insights_job(&app, &creds, &query, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn insights_job(
        &self,
        key: &AccountKey,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        let job_id = job_id.to_string();
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let job_id = job_id.clone();
            Box::pin(async move { source.insights_job(&app, &creds, &job_id, deadline).await })
        })
        .await
    }

    pub async fn insights_job_result(
        &self,
        key: &AccountKey,
        job_id: &str,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        let job_id = job_id.to_string();
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let job_id = job_id.clone();
            let query = query.clone();
            Box::pin(async move {
                source
                    .insights_job_result(&app, &creds, &job_id, &query, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn cancel_insights_job(
        &self,
        key: &AccountKey,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        let job_id = job_id.to_string();
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let job_id = job_id.clone();
            Box::pin(async move {
                source
                    .cancel_insights_job(&app, &creds, &job_id, deadline)
                    .await
            })
        })
        .await
    }

    /// Poll until the job is terminal or `deadline` fires. Pending is a
    /// successful document, not a timeout error. Same 2s cadence as ads review.
    #[cfg(feature = "meta-ads")]
    pub async fn wait_for_insights_job(
        &self,
        key: &AccountKey,
        job_id: &str,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsJobWait, Error> {
        self.wait_for_insights_job_with_interval(key, job_id, query, deadline, REVIEW_POLL_INTERVAL)
            .await
    }

    #[cfg(feature = "meta-ads")]
    pub(crate) async fn wait_for_insights_job_with_interval(
        &self,
        key: &AccountKey,
        job_id: &str,
        query: InsightsQuery,
        deadline: Deadline,
        poll_interval: std::time::Duration,
    ) -> Result<InsightsJobWait, Error> {
        loop {
            let job = self.insights_job(key, job_id, deadline).await?;
            // Meta: fetch results only when async_status is Job Completed and
            // async_percent_completion is 100.
            if job.status == InsightsJobStatus::Completed && job.percent_complete == 100 {
                let reply = self
                    .insights_job_result(key, job_id, query, deadline)
                    .await?;
                return Ok(InsightsJobWait::Completed(reply));
            }
            if matches!(
                job.status,
                InsightsJobStatus::Failed | InsightsJobStatus::Skipped
            ) {
                return Ok(InsightsJobWait::Failed(job));
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(InsightsJobWait::Pending(job));
            }
            let delay = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            tokio::time::sleep(delay).await;
        }
    }

    /// Discover remote advertising accounts for the credential. This is a
    /// read-only sibling of `insights`, not `Vault::list`: the latter returns
    /// local aliases while this call returns the platform's `act_<id>`s.
    pub async fn ad_accounts(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<AdAccountsReply, Error> {
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let source = self.insights_source(&key.site, Capability::ReadAdAccounts)?;
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            Box::pin(async move { source.ad_accounts(&app, &creds, deadline).await })
        })
        .await
    }
}
