//! Резервная копия — формат общий, из kit (`kit_core::backup`). Пароль копий нужен и облаку: ключ облака
//! выводится из него. Здесь — метка и расширение файла копии.
//!
//! **У выпущенного приложения не менять:** старые копии перестанут узнаваться.

use kit_core::backup::Format;

/// Копии «Заготовки»: метка `STARTBAK`, файлы `.starter`; своих видов записей нет.
pub const BACKUP: Format = Format { magic: b"STARTBAK", ext: "starter", names: crate::NAMES, common: &["starter", "заготовка"], records: &[] };

/// Местное значение с ключом копий (под защитой базы).
pub const META_KEY: &str = "backup.key";
