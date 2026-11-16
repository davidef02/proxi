//! Descendant process walk for the render tests (AC4: a render starts the browser, never Node).

use std::process::Command;

/// Every descendant of `pid`, transitively, as `(pid, process name)`.
pub fn descendants(pid: u32) -> Vec<(u32, String)> {
    let table = process_table();
    let mut out: Vec<(u32, String)> = Vec::new();
    let mut frontier = vec![pid];
    while let Some(parent) = frontier.pop() {
        for (child, ppid, name) in &table {
            if *ppid == parent && !out.iter().any(|(seen, _)| seen == child) {
                out.push((*child, name.clone()));
                frontier.push(*child);
            }
        }
    }
    out
}

type ProcessRow = (u32, u32, String);

#[cfg(target_os = "linux")]
fn process_table() -> Vec<ProcessRow> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let comm = stat[open + 1..close].to_string();
        let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        if let Some(ppid) = rest.get(1).and_then(|raw| raw.parse::<u32>().ok()) {
            out.push((pid, ppid, comm));
        }
    }
    out
}

#[cfg(target_os = "windows")]
fn process_table() -> Vec<ProcessRow> {
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
                  Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | \
                  ConvertTo-Json -Compress";
    let Ok(output) = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return Vec::new();
    };
    let rows = match value {
        serde_json::Value::Array(rows) => rows,
        other => vec![other],
    };
    rows.iter()
        .filter_map(|row| {
            let pid = row.get("ProcessId")?.as_u64()? as u32;
            let ppid = row.get("ParentProcessId")?.as_u64()? as u32;
            let name = row.get("Name")?.as_str()?.to_string();
            Some((pid, ppid, name))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn process_table() -> Vec<ProcessRow> {
    let Ok(output) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let pid_end = trimmed.find(char::is_whitespace)?;
            let pid = trimmed[..pid_end].parse::<u32>().ok()?;
            let rest = trimmed[pid_end..].trim_start();
            let ppid_end = rest.find(char::is_whitespace)?;
            let ppid = rest[..ppid_end].parse::<u32>().ok()?;
            let name = rest[ppid_end..].trim().to_string();
            Some((pid, ppid, name))
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn process_table() -> Vec<ProcessRow> {
    Vec::new()
}
