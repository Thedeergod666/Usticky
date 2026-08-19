// 剪贴板读取（v0.2 粘贴按钮）
//
// 三条路径，按优先级：
//   1. macOS NSPasteboard 原始字节 —— 保 GIF 动画（`public.gif` 原数据），
//      支持 Finder 复制文件（`public.file-url` 读原文件），TIFF 走 image crate
//      解码转 PNG。只有这条路径能拿到"带动画的 GIF"。
//   2. tauri-plugin-clipboard-manager read_image —— 跨平台 RGBA 兜底
//      （Win/Linux 主路径；缺点：动画 GIF 退化为首帧静态 PNG）。
//   3. 文本 —— 插件 read_text。
//
// 为什么是 Rust 端而不是前端 navigator.clipboard.read()：
//   WKWebView 在非 key window 下 Clipboard API 不可靠（跟 copyTodoText 的
//   writeText 失败同理），且 read() 需要 document focused + 权限弹窗。
//   Rust 端读 OS pasteboard 无焦点要求，零权限弹窗。
//
// 为什么图片优先于文本：复制 GIF/截图时剪贴板常同时带 alt-text / 文件名
// 文本，文本优先会让图片粘贴永远失效。纯文本复制不会带图片类型，不会误判。
use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

/// 单张贴剪贴板图片的落盘就绪数据。
pub struct ClipboardImage {
    /// 原始文件字节（gif/png/jpg）。TIFF / RGBA 路径已转成 PNG。
    pub data: Vec<u8>,
    /// 扩展名（不带点）："gif" | "png" | "jpg"
    pub ext: &'static str,
    pub mime: &'static str,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 建议标题（Finder 文件名等），None 时调用方用 i18n 占位名。
    pub name: Option<String>,
}

pub enum ClipboardContent {
    Text(String),
    Image(ClipboardImage),
    Empty,
}

/// 剪贴板图片体积上限（25MB）。超过视为异常数据，跳过图片路径。
/// GIF 动图 10-20MB 很常见，不能按"截图大小"设限。
const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;

pub fn read(app: &AppHandle) -> ClipboardContent {
    // 1. macOS 原生：GIF 原字节 / file-url / TIFF→PNG
    #[cfg(target_os = "macos")]
    if let Some(img) = macos_native::read_native_image() {
        return ClipboardContent::Image(img);
    }

    // 2. 插件 RGBA → PNG（Win/Linux 主路径，macOS 兜底）
    if let Some(img) = read_plugin_image(app) {
        return ClipboardContent::Image(img);
    }

    // 3. 文本
    match app.clipboard().read_text() {
        Ok(s) if !s.trim().is_empty() => ClipboardContent::Text(s),
        _ => ClipboardContent::Empty,
    }
}

/// 路径 2：插件 read_image → RGBA → PNG 编码。
///
/// 跨平台一致，但拿不到原格式 —— GIF 只剩首帧。macOS 上只是兜底
/// （NSPasteboard 没给出 gif/png/jpeg/tiff/file-url 时才走到这）。
fn read_plugin_image(app: &AppHandle) -> Option<ClipboardImage> {
    let img = app.clipboard().read_image().ok()?;
    let (w, h) = (img.width(), img.height());
    // **P3-3 fix**：read_image() 内部已分配 RGBA buffer，这里**至少**在
    // to_vec() 二次拷贝 + PNG 编码前预检体积，防超大剪贴板图（Win/Linux
    // 主路径）OOM。read_image 自身的内部分配护不住（在插件层），但拦掉
    // to_vec + PNG 编码这两步已能挡住绝大多数 GB 级 payload。macOS 原生
    // 路径（1a/1c/file-url）已分别 data.len()/metadata 预检，唯独插件路径漏。
    let rgba_bytes = (w as u64).saturating_mul(h as u64).saturating_mul(4) as usize;
    if rgba_bytes > MAX_IMAGE_BYTES {
        return None;
    }
    let rgba = img.rgba().to_vec();
    let raw = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut buf = std::io::Cursor::new(Vec::new());
    raw.write_to(&mut buf, image::ImageFormat::Png).ok()?;
    let data = buf.into_inner();
    if data.len() > MAX_IMAGE_BYTES {
        return None;
    }
    Some(ClipboardImage {
        data,
        ext: "png",
        mime: "image/png",
        width: Some(w),
        height: Some(h),
        name: None,
    })
}

/// 从原始字节探测图片尺寸（失败不致命，返回 None pair）。
fn probe_dims(data: &[u8]) -> (Option<u32>, Option<u32>) {
    let reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format();
    match reader {
        Ok(r) => match r.into_dimensions() {
            Ok((w, h)) => (Some(w), Some(h)),
            Err(_) => (None, None),
        },
        Err(_) => (None, None),
    }
}

#[cfg(target_os = "macos")]
mod macos_native {
    use super::{ClipboardImage, MAX_IMAGE_BYTES};
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSData, NSString, NSURL};

    /// 路径 1：NSPasteboard 原始字节。
    ///
    /// 优先级：public.gif → public.png → public.jpeg → public.file-url（Finder
    /// 复制文件）→ public.tiff（解码转 PNG）。
    ///
    /// 设计取舍：**不**先查 `types()` 再 dataForType —— dataForType 拿不到就
    /// 返 None，直接按优先级逐个试更省代码也更稳（types 列表的 NSArray
    /// 遍历在 objc2 0.6 / objc2-foundation 0.3 的 API 组合里反而啰嗦）。
    pub fn read_native_image() -> Option<ClipboardImage> {
        let pb = NSPasteboard::generalPasteboard();

        // 1a. 原始位图类型（GIF 优先 —— 保动画是本路径存在的核心理由）
        for (uti, ext, mime) in [
            ("public.gif", "gif", "image/gif"),
            ("public.png", "png", "image/png"),
            ("public.jpeg", "jpg", "image/jpeg"),
        ] {
            let t = NSString::from_str(uti);
            if let Some(data) = pb.dataForType(&t) {
                // **P2 fix（review v0.2.6）**：先 data.len() 预检，零分配地拒绝
                // 超大 pasteboard 载荷（NSPasteboard 可持有 GB 级，原始 Vec<u8>
                // 镜像会 OOM app 才能在下行 skip）。
                let len = data.len();
                if len == 0 || len > MAX_IMAGE_BYTES {
                    continue;
                }
                let bytes = nsdata_to_vec(&data);
                let (width, height) = super::probe_dims(&bytes);
                return Some(ClipboardImage {
                    data: bytes,
                    ext,
                    mime,
                    width,
                    height,
                    name: None,
                });
            }
        }

        // 1b. Finder 复制文件：public.file-url → 读原文件（限图片扩展名）。
        // 字符串形式是 "file:///..." URL，NSURL.path() 负责 percent-decode。
        let file_url_t = NSString::from_str("public.file-url");
        if let Some(url_str) = pb.stringForType(&file_url_t) {
            if let Some(img) = read_image_file_url(&url_str.to_string()) {
                return Some(img);
            }
        }

        // 1c. TIFF（macOS 截图 / Preview 复制的经典格式）：解码 → PNG。
        let tiff_t = NSString::from_str(UTI_TIFF);
        if let Some(data) = pb.dataForType(&tiff_t) {
            // **P2 fix（review v0.2.6）**：同 1a —— 先 data.len() 预检再 alloc。
            let len = data.len();
            if len == 0 || len > MAX_IMAGE_BYTES {
                // fall through to None
            } else {
                let bytes = nsdata_to_vec(&data);
                if let Some(img) = transcode_tiff_to_png(&bytes) {
                    return Some(img);
                }
            }
        }

        None
    }

    /// NSPasteboardTypeTIFF 常量在 objc2-app-kit 里是 &NSString 静态，
    /// 这里直接用 UTI 字符串，少一层符号依赖。
    const UTI_TIFF: &str = "public.tiff";

    fn nsdata_to_vec(d: &NSData) -> Vec<u8> {
        d.to_vec()
    }

    /// file:/// URL → 读图片文件。扩展名白名单 + 体积上限，非图片 / 超大
    /// 直接放弃（走后续 TIFF / 文本路径）。
    fn read_image_file_url(url_str: &str) -> Option<ClipboardImage> {
        let ns_url = NSURL::URLWithString(&NSString::from_str(url_str))?;
        let path = ns_url.path()?.to_string();
        let p = std::path::Path::new(&path);
        let ext_lower = p.extension()?.to_str()?.to_lowercase();
        let (ext, mime) = match ext_lower.as_str() {
            "gif" => ("gif", "image/gif"),
            "png" => ("png", "image/png"),
            "jpg" | "jpeg" => ("jpg", "image/jpeg"),
            _ => return None,
        };
        // **P2 fix（review v0.2.6）**：先 fs::metadata 检 size，再 std::fs::read。
        // 用户从 Finder 误粘了 GB 级 .gif/.png，std::fs::read 会整文件 slurp 进内存。
        let meta = std::fs::metadata(p).ok()?;
        if meta.len() == 0 || meta.len() > MAX_IMAGE_BYTES as u64 {
            return None;
        }
        let bytes = std::fs::read(p).ok()?;
        let (width, height) = super::probe_dims(&bytes);
        // **P3-6 fix（2026-08-13 全量审查）**：扩展名白名单只按后缀放行，一个
        // 重命名为 `x.png` 的文本文件（或非图片内容）仍会落盘成 todo。probe_dims
        // 读格式 header 取尺寸，失败 = 非 image crate 能解码的图片 -> 拒绝，
        // 走后续 TIFF / 文本路径，不生成裂图 todo。
        if width.is_none() && height.is_none() {
            return None;
        }
        let name = p.file_stem()?.to_str().map(|s| s.to_string());
        Some(ClipboardImage {
            data: bytes,
            ext,
            mime,
            width,
            height,
            name,
        })
    }

    /// TIFF 字节 → PNG（image crate 解码 + 重编码）。预览窗口 / 缩略图的
    /// <img> 不认 TIFF，必须转。
    fn transcode_tiff_to_png(bytes: &[u8]) -> Option<ClipboardImage> {
        // **P2-3 fix（2026-08-13 全量审查）**：decode 前先 probe 像素尺寸。
        // TIFF 等格式压缩比极高，~20MB 的合法 TIFF 可编码 100000×100000，
        // `reader.decode()` 会按像素分配 ~40GB RGBA buffer → OOM（release 配置
        // `panic = "abort"` 无恢复路径）。MAX_IMAGE_BYTES 只在编码字节侧拦，
        // 拦不住解码后的像素体积 —— 插件路径（read_plugin_image）已有同款
        // 预检，TIFF 路径之前漏了。consume 一次 reader 取尺寸，通过后再建一次 decode。
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        let rgba_bytes = (w as u64).saturating_mul(h as u64).saturating_mul(4);
        if rgba_bytes > MAX_IMAGE_BYTES as u64 {
            return None;
        }
        let img = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?;
        let (w, h) = (img.width(), img.height());
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
        let data = buf.into_inner();
        if data.len() > MAX_IMAGE_BYTES {
            return None;
        }
        Some(ClipboardImage {
            data,
            ext: "png",
            mime: "image/png",
            width: Some(w),
            height: Some(h),
            name: None,
        })
    }
}
