// The app's identity: every name the app, its data folder and its integrations use. Plain ESM so Electron main, the
// shortcut installer and the postinstall script load it without a TS loader.

// visible name
export const APP_NAME = "Proxi";
// identifiers: Electron app name, so the profile is %APPDATA%\proxi; package name
export const APP_ID = "proxi";
// Windows AppUserModelId, Company.Product
export const APP_USER_MODEL_ID = "DavideF02.Proxi";
// Anki root deck: renaming it orphans every synced deck
export const DECK_ROOT = "Proxi";
// default data folder under Documents, GitHub repo, Drive folder
export const DATA_DIR_NAME = "proxi-data";
export const SHORTCUT_NAME = "Proxi.lnk";
// skips the embedding model download in postinstall
export const SKIP_MODEL_ENV = "PROXI_SKIP_MODEL";
// Drive appProperties key marking our folder/files
export const DRIVE_APP_PROPERTY = "proxi";
