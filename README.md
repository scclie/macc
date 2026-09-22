# macc

self-service matrix account portal. user proves who they are via oidc and
creates their matrix account or resets its password. no admin bot to bother.

## why

matrix auth is stuck between msc3861 (oidc) and msc3824 (password upgrade).
homeservers pick sides, clients defer. continuwuity went oidc-only, dropped
sso, has no admin http api. so this is a crutch: the login side is your oidc
provider (kanidm, via oauth2-proxy in front of macc), the account side is the
admin room with `!admin users create` (continuwuity).

macc itself never speaks oidc, it only reads the proxy headers
(`X-Auth-Request-Preferred-Username`, `X-Auth-Request-Groups`). matrix never
sees oidc either, it gets a plain password through the admin room.

## how

oidc user -> oauth2-proxy -> macc(127.0.0.1) -> homeserver c-s api -> `!admin users create/reset-password` in `#admins` -> poll the room for the reply

identity comes from proxy headers, never from the client. no db, the
homeserver is the state.

## run

```sh
cargo build --release   # or nix build, or the docker image from the CI

HS=http://10.0.0.19:6167 \
HS_DOMAIN=example.org \
ADMIN_TOKEN=... \
ADMIN_USER=@maccbot:example.org \
ADMIN_ROOM=#admins:example.org \
./target/release/macc
```

only binds 127.0.0.1. put nginx + oauth2-proxy in front, the whole auth is
them:

```
X-Auth-Request-Preferred-Username: bob
X-Auth-Request-Groups: matrix_users
```

optional env: `WEB_URL` (element/cinny link on the page), `ALLOWED_GROUP`,
`PORT` (8787), `PASSWORD_MIN` (10).

## homeserver pre-req (once) (continuwuity)

- `!admin users create maccbot <password>`, invite it to the admin room
- `!admin users issue-token maccbot <password>` -> that's `ADMIN_TOKEN`

## limitations

- waits for the bot's reply and greps for "successfully"
- no token issuance flow, password is enough for cinny/element

# license
MIT
