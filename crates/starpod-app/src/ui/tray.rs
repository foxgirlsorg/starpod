//! The tray menu's helpers: the proxy environment command the menu copies
//! (ARCHITECTURE §6.4). The menu itself is declared in `tray.slint`.

/// The shell command that points a terminal at the running proxy.
///
/// The HTTP and HTTPS proxies use the mixed port, which serves HTTP and SOCKS.
/// The SOCKS5 proxy uses the SOCKS port. Windows `cmd` gets `set` lines;
/// other shells get one `export` line.
pub fn proxy_env_command(windows: bool, http_port: u16, socks_port: u16) -> String {
    if windows {
        format!(
            "set HTTP_PROXY=http://127.0.0.1:{http_port}\r\nset HTTPS_PROXY=http://127.0.0.1:{http_port}"
        )
    } else {
        format!(
            "export http_proxy=http://127.0.0.1:{http_port} https_proxy=http://127.0.0.1:{http_port} all_proxy=socks5://127.0.0.1:{socks_port}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_command_sets_the_http_proxy_variables() {
        let command = proxy_env_command(true, 7890, 7891);
        assert!(command.starts_with("set HTTP_PROXY=http://127.0.0.1:7890"));
        assert!(command.contains("set HTTPS_PROXY=http://127.0.0.1:7890"));
        assert!(!command.contains("socks"));
    }

    #[test]
    fn the_unix_command_exports_http_and_socks_proxies() {
        assert_eq!(
            proxy_env_command(false, 7890, 7891),
            "export http_proxy=http://127.0.0.1:7890 https_proxy=http://127.0.0.1:7890 all_proxy=socks5://127.0.0.1:7891"
        );
    }
}
