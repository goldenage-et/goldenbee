pub(crate) fn read_stdin_secret(label: &str) -> Result<String, String> {
    use std::io::{IsTerminal, Read};
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        let value = read_hidden_secret(label)?;
        let value = value.trim().to_string();
        if value.is_empty() {
            return Err(format!("gb-client: {label} must not be empty"));
        }
        return Ok(value);
    }
    let mut value = String::new();
    stdin
        .read_to_string(&mut value)
        .map_err(|_| format!("gb-client: {label} could not be read from stdin"))?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(format!("gb-client: {label} must not be empty"));
    }
    Ok(value)
}

#[cfg(unix)]
pub(crate) fn read_hidden_secret(label: &str) -> Result<String, String> {
    use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
    use std::io::{BufRead, Write};
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    let original = tcgetattr(stdin.as_fd())
        .map_err(|_| format!("gb-client: {label} terminal could not be read"))?;
    let mut hidden = original.clone();
    hidden.local_flags.remove(LocalFlags::ECHO);
    tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &hidden)
        .map_err(|_| format!("gb-client: {label} terminal could not hide input"))?;
    let mut value = String::new();
    let result = stdin.lock().read_line(&mut value);
    let _ = tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &original);
    let _ = writeln!(std::io::stderr());
    result.map_err(|_| format!("gb-client: {label} could not be read from stdin"))?;
    Ok(value)
}

#[cfg(not(unix))]
pub(crate) fn read_hidden_secret(label: &str) -> Result<String, String> {
    use std::io::Read;
    let mut value = String::new();
    std::io::stdin()
        .read_to_string(&mut value)
        .map_err(|_| format!("gb-client: {label} could not be read from stdin"))?;
    Ok(value)
}
