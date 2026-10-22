//! FD que o `cosmic-session` passa em `DAEMON_NOTIFICATIONS_FD` para o painel falar com o daemon.
//! O painel só conecta quando há applet `X-NotificationsApplet` (hoje nenhum), então o socket é
//! segurado aberto, com `FD_CLOEXEC`, sem servir: a Central aciona ações por `InvokeAction` no
//! barramento de sessão.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

const ENV: &str = "DAEMON_NOTIFICATIONS_FD";

/// Se `DAEMON_NOTIFICATIONS_FD` existir e for um socket: marca `FD_CLOEXEC` e o segura.
/// Chamar antes de abrir qualquer outro fd.
pub fn hold_panel_fd() -> Option<OwnedFd> {
    let raw = std::env::var(ENV).ok()?;
    match raw.trim().parse::<RawFd>() {
        Ok(fd) if fd >= 0 => hold(fd),
        _ => {
            eprintln!("notificacoes: {ENV}={raw:?} não é um fd; ignorado");
            None
        }
    }
}

/// Toma posse do fd só se for socket; qualquer outra coisa fica intocada (nem é fechada).
fn hold(fd: RawFd) -> Option<OwnedFd> {
    // SAFETY: `stat` é POD; zero é um valor válido e `fstat` o preenche.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fstat` só lê o fd e escreve em `st`, que vive até o fim da chamada.
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        eprintln!("notificacoes: {ENV}={fd}: {}; ignorado", std::io::Error::last_os_error());
        return None;
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        eprintln!("notificacoes: {ENV}={fd} não é socket; ignorado");
        return None;
    }
    // SAFETY: `fcntl` com F_GETFD/F_SETFD sobre um fd aberto (conferido pelo `fstat`).
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        eprintln!("notificacoes: {ENV}={fd}: FD_CLOEXEC falhou: {}", std::io::Error::last_os_error());
    }
    // SAFETY: o fd foi herdado para o daemon e ninguém mais neste processo o usa.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn cloexec(fd: RawFd) -> bool {
        unsafe { libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC != 0 }
    }

    #[test]
    fn socket_is_held_with_cloexec() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) }, 0);
        assert!(!cloexec(fds[0]));
        let held = hold(fds[0]).expect("socket deveria ser segurado");
        assert_eq!(held.as_raw_fd(), fds[0]);
        assert!(cloexec(fds[0]));
        unsafe { libc::close(fds[1]) };
    }

    #[test]
    fn pipe_is_not_taken() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert!(hold(fds[0]).is_none());
        // Continua aberto: quem não é socket não é nosso.
        assert!(unsafe { libc::fcntl(fds[0], libc::F_GETFD) } >= 0);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn closed_fd_is_ignored() {
        assert!(hold(987_654).is_none());
    }
}
