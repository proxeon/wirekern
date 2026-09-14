//! Single `AdsManager` impl. Rust forbids splitting one trait impl across
//! sibling modules (`E0119`); domain work stays in create/creatives/
//! inventory/lifecycle as `pub(super)` functions.

use crate::ads::{
    AdReviewStatus, AdReviewStatusRequest, AdVideoStatus, AdVideoStatusRequest, AdsDuplicateReply,
    AdsDuplicateRequest, AdsInspectReply, AdsInspectRequest, AdsInventoryReply,
    AdsInventoryRequest, AdsStatusUpdateRequest, AdsTokenInspection, AdsTokenKind,
    CreateAdCreativeRequest, CreateLinkAdCreativeRequest, CreatePausedAdRequest,
    CreateVideoAdCreativeRequest, CreatedAd, CreatedAdCreative, CreativePreview,
    CreativePreviewRequest, MarketingApiAccessTier, UploadAdImageRequest, UploadAdVideoRequest,
    UploadedAdImage, UploadedAdVideo,
};
use crate::error::Error;
use crate::facets::AdsManager;
use crate::types::{AccountCreds, AppConfig, Deadline};
use async_trait::async_trait;
use serde_json::Value;

use super::accounts::first_ad_account;
use super::auth::{
    debug_token, marketing_api_access_tier, refuse_non_system_user_debug, require_oauth,
    system_user_creds, whoami,
};
use super::create::{create_paused_ad, read_ad_video_status, upload_ad_image, upload_ad_video};
use super::creatives::{
    create_link_ad_creative, create_typed_ad_creative, create_video_ad_creative,
    preview_ad_creative,
};
use super::graph::{
    access_token, account_id, extra_string, nonempty_value_string, read_ad_json_field,
};
use super::inventory::{inspect_ads_object, list_ads_inventory};
use super::lifecycle::{
    duplicate_ad_object, post_ad_update, read_ad_review_status, update_ad_status,
};
use super::MetaAds;

#[async_trait]
impl AdsManager for MetaAds {
    async fn create_paused_ad(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreatePausedAdRequest,
        deadline: Deadline,
    ) -> Result<CreatedAd, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_paused_ad(
            &self.http,
            &self.base,
            &self.site,
            &account,
            token,
            &request.create,
            deadline,
        )
        .await
    }

    async fn upload_ad_image(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &UploadAdImageRequest,
        deadline: Deadline,
    ) -> Result<UploadedAdImage, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        upload_ad_image(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn upload_ad_video(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &UploadAdVideoRequest,
        deadline: Deadline,
    ) -> Result<UploadedAdVideo, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        upload_ad_video(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn ad_video_status(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdVideoStatusRequest,
        deadline: Deadline,
    ) -> Result<AdVideoStatus, Error> {
        let token = access_token(creds)?;
        read_ad_video_status(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn create_link_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreateLinkAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_link_ad_creative(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn create_video_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreateVideoAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_video_ad_creative(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn create_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreateAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_typed_ad_creative(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn preview_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreativePreviewRequest,
        deadline: Deadline,
    ) -> Result<CreativePreview, Error> {
        let token = access_token(creds)?;
        preview_ad_creative(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn list_ads_inventory(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsInventoryRequest,
        deadline: Deadline,
    ) -> Result<AdsInventoryReply, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        list_ads_inventory(
            &self.http,
            &self.base,
            &self.site,
            &account,
            token,
            request.kind,
            deadline,
        )
        .await
    }

    async fn inspect_ads_object(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsInspectRequest,
        deadline: Deadline,
    ) -> Result<AdsInspectReply, Error> {
        let token = access_token(creds)?;
        inspect_ads_object(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn read_ad_targeting_json(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        id: &str,
        deadline: Deadline,
    ) -> Result<Value, Error> {
        let token = access_token(creds)?;
        read_ad_json_field(
            &self.http,
            &self.base,
            &self.site,
            token,
            id,
            "targeting",
            deadline,
        )
        .await
    }

    async fn read_special_ad_categories(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        id: &str,
        deadline: Deadline,
    ) -> Result<Vec<String>, Error> {
        let token = access_token(creds)?;
        let value = read_ad_json_field(
            &self.http,
            &self.base,
            &self.site,
            token,
            id,
            "special_ad_categories",
            deadline,
        )
        .await?;
        Ok(value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| nonempty_value_string(Some(item)))
                    .filter(|item| item != "NONE")
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn bootstrap_system_user_token(
        &self,
        app: &AppConfig,
        token: &str,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        let token = token.trim();
        if token.is_empty() {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "missing_token".into(),
            });
        }
        let oauth = require_oauth(app)?;
        // Classify before vault write so a user token cannot be stored as
        // an unattended secret (references item 3).
        let debug = debug_token(
            &self.http,
            &self.base,
            oauth,
            token,
            deadline,
            AdsTokenKind::SystemUser,
        )
        .await?;
        if !debug.is_valid {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "invalid_token".into(),
            });
        }
        refuse_non_system_user_debug(&self.site, debug.debug_type.as_deref(), debug.expires_at)?;
        if let Some(app_id) = debug.app_id.as_deref() {
            if app_id != oauth.client_id {
                return Err(Error::Auth {
                    site: self.site.clone(),
                    reason: "token_app_mismatch".into(),
                });
            }
        }
        let me = whoami(&self.http, &self.base, token, deadline).await?;
        let account = first_ad_account(&self.http, &self.base, token, deadline)
            .await?
            .ok_or_else(|| Error::Auth {
                site: self.site.clone(),
                reason: "no_ad_account".into(),
            })?;
        Ok(system_user_creds(token, &me.id, &account, debug.expires_at))
    }

    async fn inspect_access_token(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AdsTokenInspection, Error> {
        let oauth = require_oauth(app)?;
        let token = access_token(creds)?;
        let kind = AdsTokenKind::from_vault_extra(extra_string(creds, "token_kind").as_deref());
        debug_token(&self.http, &self.base, oauth, token, deadline, kind).await
    }

    async fn marketing_api_access_tier(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<MarketingApiAccessTier, Error> {
        let token = access_token(creds)?;
        marketing_api_access_tier(&self.http, &self.base, token, deadline).await
    }

    async fn ad_review_status(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdReviewStatusRequest,
        deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        let token = access_token(creds)?;
        read_ad_review_status(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn update_ad_status(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsStatusUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        let token = access_token(creds)?;
        update_ad_status(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn duplicate_ad_object(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsDuplicateRequest,
        deadline: Deadline,
    ) -> Result<AdsDuplicateReply, Error> {
        let token = access_token(creds)?;
        duplicate_ad_object(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn post_ad_update(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        id: &str,
        fields: &[(String, String)],
        deadline: Deadline,
    ) -> Result<(), Error> {
        let token = access_token(creds)?;
        post_ad_update(
            &self.http, &self.base, &self.site, token, id, fields, deadline,
        )
        .await
    }
}
