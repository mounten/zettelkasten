use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

// Icons beyond the default component set, embedded selectively.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        StickyNote,
        ListTodo,
        Code,
        Tag,
        Hash,
        Trash,
        Pin,
        PinOff,
        ClipboardPaste,
        X,
        Layers,
        NotebookPen,
        Sparkles,
        Square,
        SquareCheck,
        HardDrive,
        Vault,
        FolderPlus,
        LogOut,
    ]
);

/// Default component icons plus our extras.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
