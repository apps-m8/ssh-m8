use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use std::{
    fs::OpenOptions,
    io::{BufRead, Write},
    os::fd::AsRawFd,
};

// Keep the original service ID so existing passwords remain accessible.
const SERVICE: &str = "local.ssh-manager.password";

pub fn set(account: &str, password: &str) -> Result<(), String> {
    set_bytes(account, password.as_bytes())
}

pub fn set_bytes(account: &str, password: &[u8]) -> Result<(), String> {
    set_generic_password(SERVICE, account, password)
        .map_err(|e| format!("Could not save the password in macOS Keychain: {e}"))
}

pub fn get(account: &str) -> Result<Option<Vec<u8>>, String> {
    match get_generic_password(SERVICE, account) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.code() == -25300 => Ok(None),
        Err(e) => Err(format!("Could not read the saved password: {e}")),
    }
}

pub fn delete(account: &str) -> Result<(), String> {
    match delete_generic_password(SERVICE, account) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == -25300 => Ok(()),
        Err(e) => Err(format!("Could not remove the saved password: {e}")),
    }
}

fn is_password_prompt(prompt: &str) -> bool {
    let prompt = prompt.trim().to_ascii_lowercase();
    prompt == "password:" || prompt.ends_with("'s password:") || prompt.ends_with(") password:")
}

// SSH invokes this executable directly; passwords never enter argv, env, files, or logs.
pub fn askpass() -> Result<(), String> {
    let prompt = std::env::args().nth(2).unwrap_or_default();
    if std::env::var("SSH_ASKPASS_PROMPT").as_deref() == Ok("none") {
        eprintln!("{prompt}");
        return Ok(());
    }
    if is_password_prompt(&prompt) {
        let account =
            std::env::var("SSH_M8_CREDENTIAL_ACCOUNT").map_err(|_| "Missing credential account")?;
        let mut password = get_generic_password(SERVICE, &account)
            .map_err(|e| format!("Could not read the saved password: {e}"))?;
        let result = std::io::stdout().write_all(&password);
        // Volatile writes keep the clear from being optimized away.
        for byte in &mut password {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        result.map_err(|e| e.to_string())?;
        return Ok(());
    }
    if std::env::var("SSH_M8_INTERACTIVE").as_deref() != Ok("1") {
        return Err("This prompt needs an interactive terminal. Connect from Hosts first.".into());
    }
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| "Open a terminal connection to answer the SSH prompt.")?;
    write!(tty, "{prompt} ")
        .and_then(|_| tty.flush())
        .map_err(|e| e.to_string())?;
    let fd = tty.as_raw_fd();
    let mut previous = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, previous.as_mut_ptr()) } != 0 {
        return Err("Cannot read terminal settings.".into());
    }
    let previous = unsafe { previous.assume_init() };
    let mut hidden = previous;
    hidden.c_lflag &= !libc::ECHO;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
        return Err("Cannot hide terminal input.".into());
    }
    let mut answer = String::new();
    let result = std::io::BufReader::new(&tty).read_line(&mut answer);
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, &previous);
    }
    let _ = writeln!(tty);
    result.map_err(|e| e.to_string())?;
    if std::env::var("SSH_ASKPASS_PROMPT").as_deref() == Ok("confirm") {
        if !matches!(answer.trim(), "yes" | "y") {
            return Err("Confirmation declined.".into());
        }
    } else {
        print!("{}", answer.trim_end_matches(['\r', '\n']));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Writes and removes a disposable macOS Keychain credential"]
    fn keychain_roundtrip() {
        let account = format!("ssh-m8-test-{}", std::process::id());
        set(&account, "disposable-test-value").unwrap();
        let result = get(&account);
        delete(&account).unwrap();
        assert_eq!(result.unwrap().unwrap(), b"disposable-test-value");
        assert!(get(&account).unwrap().is_none());
    }
    #[test]
    fn passwords_are_not_sent_to_fingerprint_passphrase_or_mfa_prompts() {
        assert!(is_password_prompt("root@example.org's password: "));
        assert!(is_password_prompt("Password:"));
        for prompt in [
            "Enter passphrase for key:",
            "Verification code:",
            "New password:",
            "Retype password:",
            "Are you sure you want to continue connecting (yes/no)?",
        ] {
            assert!(!is_password_prompt(prompt));
        }
    }
}
