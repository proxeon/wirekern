# Threads review app

`wirekern-threads-app` is the thin, user-facing product layer needed for a Meta App Review submission. It is deliberately separate from the Wirekern CLI and library: Wirekern owns official OAuth/token refresh/publishing; this binary owns browser sessions, scheduled-post records, explicit customer approval, and account deletion.

It supports one connected Threads account per browser session and schedules **text posts only**. It requests only `threads_basic,threads_content_publish`. Reply access is not requested or used.

## What it provides

- A browser **Connect Threads** OAuth flow with server-side state validation.
- An authenticated dashboard that shows the connected handle.
- An explicit checkbox approving the exact post text and selected time before a job is created.
- A durable, owner-only file store for sessions, post text, schedules, and outcomes.
- A conservative worker: an uncertain or interrupted publish is **not retried automatically**, so a network failure cannot silently double-post.
- **Disconnect and delete data**: deletes the account credential, sessions, scheduled posts, and local idempotency records.
- `POST /data-deletion`: verifies Meta's HMAC-signed `signed_request`, deletes matching account data, and returns the required confirmation URL/code.
- A public `/privacy` page suitable as a starting point for the policy URL.

This is a single-instance service. Run one process against one persistent data directory; do not place it behind a multi-instance load balancer or a shared network filesystem without replacing its file store with a database/queue.

## Deploy

1. Choose a real HTTPS origin, such as `https://threads.example.com`. Put a reverse proxy or platform HTTPS endpoint in front of the local listener. Do not expose the plain HTTP listener directly to the public internet.

2. In Meta's **Access the Threads API** settings, set these exact URLs:

   - Redirect callback: `https://threads.example.com/auth/threads/callback`
   - Uninstall callback: `https://threads.example.com/deauthorize`
   - Data deletion callback: `https://threads.example.com/data-deletion`

   The redirect URL must be added as a saved dashboard chip and must exactly match the environment variable below.

3. Set production secrets in your deployment secret manager—not in the repository:

   ```sh
   WIREKERN_THREADS_CLIENT_ID='Threads App ID'
   WIREKERN_THREADS_CLIENT_SECRET='Threads App secret'
   WIREKERN_THREADS_REDIRECT_URI='https://threads.example.com/auth/threads/callback'
   WIREKERN_THREADS_APP_PUBLIC_URL='https://threads.example.com'
   WIREKERN_THREADS_APP_DATA_DIR='/var/lib/wirekern-threads'
   WIREKERN_THREADS_SUPPORT_EMAIL='support@example.com'
   ```

4. Build and run it behind HTTPS:

   ```sh
   cargo run -p wirekern-serve --bin wirekern-threads-app -- \
     --public-url "$WIREKERN_THREADS_APP_PUBLIC_URL" \
     --data-dir "$WIREKERN_THREADS_APP_DATA_DIR" \
     --bind 127.0.0.1:8790
   ```

   Check the local process with `GET http://127.0.0.1:8790/healthz`. The public proxy must preserve normal form POST bodies to `/data-deletion`.

5. Set `WIREKERN_THREADS_SUPPORT_EMAIL` to your real business support address before review. The app refuses to start without it, and `/privacy` displays it without requiring login.

The service refuses to start if its public origin is not HTTPS or if `WIREKERN_THREADS_REDIRECT_URI` does not exactly equal `<public-url>/auth/threads/callback`.

## Meta App Review submission

Keep the Meta app in Development mode while testing with a Threads Tester. A tester is not a production customer.

In **App Review / Permissions and features**, request Advanced Access only for:

- `threads_basic`
- `threads_content_publish`

Suggested explanation:

> The application lets an account owner connect their own Threads account and schedule text they explicitly create or approve. `threads_basic` identifies the connected account and displays its username in the dashboard. `threads_content_publish` publishes that approved text to the same connected account at the selected time. We do not read replies, followers, or insights, and we do not publish to accounts that have not authorized the application. The account owner can disconnect to delete their stored token, sessions, post content, and schedule; Meta deletion requests are processed at the configured callback.

Record an unedited screencast showing:

1. the public landing page and privacy link;
2. **Connect Threads** and the Meta consent screen;
3. the connected handle in the dashboard;
4. entering text, selecting a time, and ticking the approval checkbox;
5. the published post/result; and
6. **Disconnect and delete data**.

Do not show the Threads App secret, user token, or a Meta administrator login in the video. If the review form requires app credentials, create a dedicated non-admin test account for this product—not a personal Meta admin account.

After the requested permissions are approved, switch the app to Live mode. The actual client can then connect normally; they should not be added as a permanent Threads Tester.

## Optional replies

Reply functionality stays in Wirekern but is outside this review app's scope. If you later add a real reply-management screen, request `threads_manage_replies`, implement it in the product UI, and re-authorize with:

```sh
wirekern auth threads --with-replies
```

Do not request it merely because the kernel can support it.
