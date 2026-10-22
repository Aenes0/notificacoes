//! Leitura de arquivos vindos de fora (caminhos do D-Bus, temas de ícones) sem risco de travar a thread.
//! `open(2)` de um FIFO em O_RDONLY bloqueia até aparecer um escritor: qualquer processo local poderia
//! congelar o daemon apontando um hint para um FIFO. Por isso abre com O_NONBLOCK e confere o tipo no fd.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Lê um arquivo REGULAR de até `max` bytes. FIFO, diretório, dispositivo → `InvalidInput`;
/// maior que `max` → `FileTooLarge`. O tipo vem do `fstat` no fd já aberto (sem corrida entre checar e abrir).
pub fn read_regular(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "não é um arquivo regular"));
    }
    if meta.len() > max {
        return Err(io::Error::new(io::ErrorKind::FileTooLarge, "arquivo grande demais"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::FileTooLarge, "arquivo grande demais"));
    }
    Ok(bytes)
}

#[cfg(test)]
pub mod testutil {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    pub fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("notificacoes-fsread-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    pub fn make_fifo(dir: &std::path::Path, name: &str) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        let p = dir.join(name);
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).expect("cstr");
        // SAFETY: `c` é uma string C válida e viva durante a chamada.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
        p
    }

    /// Roda `f` numa thread de teste; falha (em vez de travar a suíte) se passar de 1 s.
    pub fn within_1s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(1)).expect("travou: bloqueou em arquivo que não é regular")
    }
}
