//! Vigia de arquivos por inotify: acompanha o "não perturbe" e a config.
//!
//! Vigia o DIRETÓRIO (não o arquivo): o COSMIC grava por arquivo temporário + rename e o arquivo
//! `do_not_disturb` só existe depois do primeiro toggle. O fd é não bloqueante, para ficar no calloop.

use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;

/// Quem mudou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchTag {
    Dnd,
    Config,
}

/// Diretório onde o COSMIC guarda o estado do "não perturbe".
pub fn dnd_dir(cosmic_dir: &Path) -> PathBuf {
    cosmic_dir.join("com.system76.CosmicNotifications").join("v1")
}

/// Lê o "não perturbe": só `true` (aparado) liga; ausente, ilegível ou outro texto → `false`.
pub fn read_dnd(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("do_not_disturb")).is_ok_and(|s| s.trim() == "true")
}

pub struct Watcher {
    fd: OwnedFd,
    watches: Vec<(i32, WatchTag)>,
}

impl Watcher {
    /// Cria os diretórios que faltarem e passa a vigiá-los.
    pub fn new(watches: &[(PathBuf, WatchTag)]) -> io::Result<Watcher> {
        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)?;
        let flags = WatchFlags::CLOSE_WRITE | WatchFlags::MOVED_TO | WatchFlags::CREATE | WatchFlags::DELETE;
        let mut registered = Vec::with_capacity(watches.len());
        for (dir, tag) in watches {
            std::fs::create_dir_all(dir)?;
            let wd = inotify::add_watch(&fd, dir, flags)?;
            registered.push((wd, *tag));
        }
        Ok(Watcher { fd, watches: registered })
    }

    /// Tags distintas com evento desde a última chamada, na ordem em que apareceram. Nunca bloqueia.
    pub fn drain(&mut self) -> Vec<WatchTag> {
        let mut tags: Vec<WatchTag> = Vec::new();
        let mut push = |tag: WatchTag| {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        };
        let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        loop {
            match reader.next() {
                Ok(ev) => {
                    if ev.events().contains(ReadFlags::QUEUE_OVERFLOW) {
                        // Eventos perdidos: reler tudo é o único jeito seguro.
                        self.watches.iter().for_each(|(_, t)| push(*t));
                    } else if let Some((_, tag)) = self.watches.iter().find(|(wd, _)| *wd == ev.wd()) {
                        push(*tag);
                    }
                }
                Err(Errno::INTR) => continue,
                // Fila vazia (EAGAIN) ou erro de leitura: para por agora.
                Err(_) => break,
            }
        }
        tags
    }
}

impl AsFd for Watcher {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Diretório temporário próprio do teste, limpo antes de usar.
    fn temp(nome: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("notificacoes-teste-{}-{nome}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn read_dnd_values() {
        let d = temp("read-dnd");
        assert!(!read_dnd(&d), "ausente");
        fs::create_dir_all(&d).unwrap();
        for (conteudo, esperado) in [("true\n", true), ("false", false), ("lixo", false), (" true ", true)] {
            fs::write(d.join("do_not_disturb"), conteudo).unwrap();
            assert_eq!(read_dnd(&d), esperado, "conteúdo {conteudo:?}");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn dnd_dir_appends_cosmic_path() {
        assert_eq!(
            dnd_dir(Path::new("/x")),
            PathBuf::from("/x/com.system76.CosmicNotifications/v1")
        );
    }

    #[test]
    fn change_in_watched_dir_is_reported() {
        let d = temp("change");
        fs::create_dir_all(&d).unwrap();
        let mut w = Watcher::new(&[(d.clone(), WatchTag::Dnd)]).unwrap();
        assert!(w.drain().is_empty(), "sem eventos ainda; não bloqueia");
        fs::write(d.join(".tmp123"), "true").unwrap();
        fs::rename(d.join(".tmp123"), d.join("do_not_disturb")).unwrap();
        assert_eq!(w.drain(), vec![WatchTag::Dnd]);
        assert!(w.drain().is_empty(), "eventos consumidos");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_dir_is_created_and_watched() {
        let base = temp("missing");
        let d = base.join("a").join("b");
        let mut w = Watcher::new(&[(d.clone(), WatchTag::Config)]).unwrap();
        assert!(d.is_dir());
        fs::write(d.join("config.toml"), "x").unwrap();
        assert_eq!(w.drain(), vec![WatchTag::Config]);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn two_tags_reported_once_each() {
        let base = temp("two");
        let (a, b) = (base.join("dnd"), base.join("cfg"));
        let mut w = Watcher::new(&[(a.clone(), WatchTag::Dnd), (b.clone(), WatchTag::Config)]).unwrap();
        for i in 0..3 {
            fs::write(a.join(format!("f{i}")), "x").unwrap();
            fs::write(b.join(format!("g{i}")), "x").unwrap();
        }
        let mut tags = w.drain();
        tags.sort_by_key(|t| *t as u8);
        assert_eq!(tags, vec![WatchTag::Dnd, WatchTag::Config]);
        let _ = fs::remove_dir_all(&base);
    }
}
