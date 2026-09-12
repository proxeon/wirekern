# Meta Ads creatives slice

Implementation notes for the remaining Assets/creatives checklist rows.
Validated against [008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
items 16–18, the [Ad Creative reference](https://developers.facebook.com/docs/marketing-api/reference/ad-creative/),
[CTA value](https://developers.facebook.com/docs/marketing-api/reference/ad-creative-link-data-call-to-action-value/),
[video ads](https://developers.facebook.com/docs/marketing-api/guides/videoads/),
[Advantage+ standard enhancements](https://developers.facebook.com/docs/marketing-api/advantage-catalog-ads/standard-enhancements/),
and the [v26.0 WhatsApp Status notes](https://developers.facebook.com/blog/post/2026/07/29/introducing-graph-api-v26-and-marketing-api-v26/)
(2026-09-12). Graph pin stays `v26.0`. Creatives have no delivery status;
only a later ad is `PAUSED`.

Product rule: closed types, extra CTA value fields modelled, no Graph JSON
hatch. A misspelled CTA or a video used before `ready` must fail before HTTP.

## 1. Additional image-link CTAs

Keep `learn_more` as the default string in manifests. Add Meta types that
need more than `value.link = destination_url`:

- Website types (`shop_now`, `sign_up`, `download`, `apply_now`, `book_now`,
  `subscribe`, `buy_now`, `contact_us`, `get_quote`, `order_now`) still send
  `value.link` as the destination HTTPS URL.
- `like_page` / `call_now` / `whatsapp_message` send `value.page = page_id`.
- `get_directions` requires `--geo-link` (`fbgeo://…` or maps HTTPS).
- `install_app` requires `--application-id` (numeric) and `--app-link`.

Unlisted CTA names fail `unknown_link_call_to_action` locally.

## 2. Video upload + processing poll

`POST /act_{id}/advideos` multipart `source` (basename only, same path
redaction as images). Reply is a numeric `video_id`. Encoding is async:
`GET /{video_id}?fields=status` until `status.video_status` is `ready`,
`error`, or the deadline. Poll every 2s (same cadence as ads review).
Pending at deadline is a successful pending reply, not a delivery claim.

## 3. Video creative

`object_story_spec.video_data`: `video_id`, thumbnail `image_hash`,
`message`, CTA. Thumbnail is required locally so Meta does not invent one.

## 4. Carousel

`link_data.child_attachments`: 2–10 cards (Meta's documented range; 3+ is
recommended). Each card: `image_hash`, HTTPS `link`, `name`. Parent
`message` + CTA apply to the unit.

## 5. Catalog / dynamic

`object_story_spec.template_data` with numeric `product_set_id`, HTTPS
`link`, `message`, CTA. Catalog itself is not created here.

## 6. Lead-form creative

Image-link shape whose CTA value includes numeric `lead_gen_form_id`.
The form is a pre-existing Page asset; Postkit does not create lead forms
(that is a separate product, listed under Not this kernel).

## 7. App-install creative

`INSTALL_MOBILE_APP` CTA with numeric `application_id` and HTTPS
`object_store_url` / `app_link`. Page identity remains explicit.

## 8. Instagram actor

Optional numeric `instagram_user_id` on `object_story_spec` (not the
deprecated `instagram_actor_id`). Omitted means Page-only identity.

## 9. Advantage+ creative assembly

Optional `--advantage-plus` sets
`degrees_of_freedom_spec.creative_features_spec.standard_enhancements.enroll_status`
to `OPT_IN`. Default is omit (Meta does not auto-enroll). This is not
Advantage+ Shopping/App campaign create (removed v25.0+).

## 10. Ads in WhatsApp Status (v26.0)

Targeting: `publisher_platforms` may include `whatsapp`;
`whatsapp_positions` is `status` only. Third-party creatives must send
`wamo_whatsapp_identity_spec` (`wamo_whatsapp_identity_id` numeric, optional
`whatsapp_phone_number`). `user_age_unknown` is explicit when Status is
selected so Meta's default `true` cannot silently expand the audience.
