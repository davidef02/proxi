// The Windows user PATH lives in HKCU\Environment; these functions edit that string, not the process PATH,
// so tests can exercise them without touching the real one. Plain ESM so the installer loads it without a TS loader.
import path from "node:path";

// PowerShell single-quoted literal: the only escape is a doubled single quote.
export const psQuote = (s) => "'" + s.replace(/'/g, "''") + "'";

// Entries compare without case or trailing separators; %VAR% entries compare literally, never expanded.
const norm = (s) => path.win32.normalize(s.trim()).replace(/[\\/]+$/, "").toLowerCase();
const same = (a, b) => norm(a) === norm(b);

export function withPathEntry(current, dir) {
  const entries = current.split(";").filter((e) => e.trim() !== "");
  if (entries.some((e) => same(e, dir))) return { value: current, changed: false };
  return { value: [...entries, dir].join(";"), changed: true };
}

export function withoutPathEntry(current, dir) {
  const entries = current.split(";").filter((e) => e.trim() !== "");
  const kept = entries.filter((e) => !same(e, dir));
  if (kept.length === entries.length) return { value: current, changed: false };
  return { value: kept.join(";"), changed: true };
}

export function readUserPath(ps) {
  // Windows PowerShell 5.1 writes stdout in the OEM code page: without this prefix a path such as
  // C:\Città\bin comes back with U+FFFD, and writing that value back would corrupt the user's PATH.
  return ps(
    "[Console]::OutputEncoding=[Text.Encoding]::UTF8;" +
      "(Get-Item -LiteralPath 'HKCU:\\Environment').GetValue('Path', '', 'DoNotExpandEnvironmentNames')",
  );
}

export function writeUserPath(ps, value) {
  // A U+FFFD means the value was decoded with the wrong code page: refuse rather than silently
  // replacing the user's unrelated PATH entries with broken ones.
  if (value.includes("\uFFFD"))
    throw new Error(
      "user PATH contains U+FFFD (decoding failure): refusing to write it back; fix the PATH by hand in Settings > Environment Variables",
    );
  // ExpandString keeps %VAR% entries expanding (the .NET Framework setter would write REG_SZ);
  // the SetEnvironmentVariable pair broadcasts WM_SETTINGCHANGE so new shells pick the PATH up.
  ps(
    "$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true);" +
      `$k.SetValue('Path',${psQuote(value)},[Microsoft.Win32.RegistryValueKind]::ExpandString);$k.Close();` +
      "[Environment]::SetEnvironmentVariable('PROXI_PATH_REFRESH','1','User');" +
      "[Environment]::SetEnvironmentVariable('PROXI_PATH_REFRESH',$null,'User')",
  );
}
