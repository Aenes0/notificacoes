//! Servidor D-Bus: protocolo puro (`server`) e a conexão no calloop (`conn`); o logind no
//! barramento de sistema (`logind`).

pub mod conn;
pub mod logind;
pub mod server;
