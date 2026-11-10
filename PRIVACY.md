# Privacy policy

Proxi is a local desktop app. It has no server of its own and collects no data.

## What the app accesses

- **Google Drive** (optional sync): only the `https://www.googleapis.com/auth/drive.file` scope, i.e. files and folders the app itself creates. It cannot read the rest of your Drive.
- **GitHub** (optional sync): the `repo` scope, used only to read and write the private `proxi-data` repository the app creates in your account.

## Where data goes

- Your study data (courses, decks, progress) is synced only between your computer and the provider you choose (your own Google Drive or your own GitHub repository).
- OAuth tokens are stored only on your computer (encrypted with the OS keychain via Electron `safeStorage` when available) and are never sent anywhere else.
- No analytics, no tracking, no data shared with third parties.

## Revoking access

Use "Disconnetti" in Settings → Sincronizzazione, or revoke the app in your
[Google account permissions](https://myaccount.google.com/permissions) / [GitHub authorized apps](https://github.com/settings/applications).

## Contact

Open an issue at https://github.com/davidef02/proxi/issues.
