//! 登录加密与正版验证（参考 master 分支 `core/src/net/authentication.rs` 与
//! `core/src/server/key_store.rs`）。
//!
//! 流程（对照 vanilla `ServerLoginNetworkHandler`）：
//! 1. 服务端生成 RSA-1024 密钥对，发 `LoginHelloS2CPacket`（空 `server_id`、SPKI DER
//!    公钥、随机 4 字节 verify token）；
//! 2. 客户端回 `LoginKeyC2SPacket`：RSA 加密的共享密钥 + RSA 加密的 token；
//! 3. 服务端 RSA 解密共享密钥、校验 token，此后读写启用 AES-128-CFB8 流加密
//!    （key 与 IV 均为共享密钥）；
//! 4. 正版模式：计算 `server_hash = SHA-1(共享密钥 ‖ RSA公钥DER)` 的
//!    `BigInteger` 十六进制（无符号、无前导零），请求 Mojang session server
//!    `hasJoined?username=<name>&serverId=<hash>` 校验；返回的 profile 提供
//!    真实 UUID 与皮肤属性。

use std::fmt;
use std::io::{self, Read, Write};

use aes::Aes128;
use aes::cipher::{Block, BlockEncrypt, KeyInit};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use num_bigint::BigInt;
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use serde::Deserialize;
use sha1::Sha1;
use sha2::Digest;
use uuid::Uuid;

/// 加密错误。
#[derive(Debug)]
pub enum CryptoError {
    /// RSA 解密失败。
    Decrypt,
    /// 共享密钥长度非法（必须 16 字节）。
    SecretLength(usize),
    /// 加密已启用（重复设置）。
    AlreadyEncrypted,
    /// 网络 IO 错误。
    Io(io::Error),
    /// Mojang session server 请求失败。
    AuthRequest,
    /// 用户名未验证（204 No Content）。
    UnverifiedUsername,
    /// session server 返回未知状态码。
    AuthStatus(u16),
    /// 响应 JSON 解析失败。
    Parse,
    /// 玩家已被封禁。
    Banned,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decrypt => f.write_str("failed to decrypt shared secret"),
            Self::SecretLength(len) => write!(f, "shared secret length {len} != 16"),
            Self::AlreadyEncrypted => f.write_str("encryption already enabled"),
            Self::Io(err) => write!(f, "io: {err}"),
            Self::AuthRequest => f.write_str("authentication servers are down"),
            Self::UnverifiedUsername => f.write_str("failed to verify username"),
            Self::AuthStatus(code) => write!(f, "unexpected auth status code {code}"),
            Self::Parse => f.write_str("failed to parse profile"),
            Self::Banned => f.write_str("you are banned from authentication servers"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// RSA 密钥对（服务端登录加密）。
pub struct KeyStore {
    private_key: RsaPrivateKey,
    public_key_der: Vec<u8>,
}

impl KeyStore {
    /// 生成 RSA-1024 密钥对并导出公钥 DER。
    pub fn new() -> Result<Self, CryptoError> {
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 1024).map_err(|_| CryptoError::Decrypt)?;
        let public_key = private_key.to_public_key();
        let public_key_der = public_key
            .to_public_key_der()
            .map_err(|_| CryptoError::Decrypt)?
            .into_vec();
        Ok(Self {
            private_key,
            public_key_der,
        })
    }

    /// SPKI DER 公钥（发往客户端）。
    #[must_use]
    pub fn public_key_der(&self) -> &[u8] {
        &self.public_key_der
    }

    /// RSA 解密客户端载荷（共享密钥或 verify token）。
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.private_key
            .decrypt(Pkcs1v15Encrypt, data)
            .map_err(|_| CryptoError::Decrypt)
    }

    /// 计算 server hash（`sha1(secret ‖ publicKeyDer)` 的无符号十六进制）。
    #[must_use]
    pub fn server_hash(&self, secret: &[u8]) -> String {
        let digest = Sha1::new()
            .chain_update(secret)
            .chain_update(&self.public_key_der)
            .finalize();
        auth_digest(&digest)
    }
}

/// SHA-1 摘要 → Mojang server hash（对照 Java `new BigInteger(bytes).toString(16)`：
/// 有符号、无前导零；首字节 ≥ 0x80 时为负数带 `-` 前缀）。
#[must_use]
pub fn auth_digest(bytes: &[u8]) -> String {
    BigInt::from_signed_bytes_be(bytes).to_str_radix(16)
}

/// AES-128-CFB8 流加密（Minecraft 登录加密，key = IV = 共享密钥）。
///
/// 对照 vanilla `NetworkEncryptionUtils`：`AES/CFB8/NoPadding`，初始向量与
/// 密钥相同。CFB8 逐字节：`C_i = AES_K(IV_i)[0] XOR P_i`，随后
/// `IV_{i+1} = (IV_i << 8) | C_i`；解密用同样的 keystream。
#[derive(Clone)]
pub struct AesCfb8 {
    cipher: Aes128,
    iv: [u8; 16],
}

impl AesCfb8 {
    /// 新建（key = IV）。
    #[must_use]
    pub fn new(key: &[u8; 16]) -> Self {
        use aes::cipher::generic_array::GenericArray;
        let key_block = GenericArray::from_slice(key);
        Self {
            cipher: Aes128::new(key_block),
            iv: *key,
        }
    }

    /// 加密一个字节并推进状态。
    fn encrypt_byte(&mut self, plain: u8) -> u8 {
        let mut block = Block::<Aes128>::default();
        block.copy_from_slice(&self.iv);
        self.cipher.encrypt_block(&mut block);
        let out = plain ^ block[0];
        self.iv.rotate_left(8);
        self.iv[15] = out;
        out
    }

    /// 解密一个字节并推进状态。
    fn decrypt_byte(&mut self, ciphertext: u8) -> u8 {
        let mut block = Block::<Aes128>::default();
        block.copy_from_slice(&self.iv);
        self.cipher.encrypt_block(&mut block);
        let out = ciphertext ^ block[0];
        self.iv.rotate_left(8);
        self.iv[15] = ciphertext;
        out
    }

    /// 批量加密（就地）。
    pub fn encrypt(&mut self, buf: &mut [u8]) {
        for byte in buf {
            *byte = self.encrypt_byte(*byte);
        }
    }

    /// 批量解密（就地）。
    pub fn decrypt(&mut self, buf: &mut [u8]) {
        for byte in buf {
            *byte = self.decrypt_byte(*byte);
        }
    }
}

/// AES-128-CFB8 流加密写入器。
pub struct EncryptingWriter<W: Write> {
    cipher: AesCfb8,
    inner: W,
}

impl<W: Write> EncryptingWriter<W> {
    /// 以共享密钥包装写入器。
    pub fn new(inner: W, key: &[u8; 16]) -> Self {
        Self {
            cipher: AesCfb8::new(key),
            inner,
        }
    }
}

impl<W: Write> Write for EncryptingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut encrypted = buf.to_vec();
        self.cipher.encrypt(&mut encrypted);
        self.inner.write_all(&encrypted)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// AES-128-CFB8 流解密读取器。
pub struct DecryptingReader<R: Read> {
    cipher: AesCfb8,
    inner: R,
}

impl<R: Read> DecryptingReader<R> {
    /// 以共享密钥包装读取器。
    pub fn new(inner: R, key: &[u8; 16]) -> Self {
        Self {
            cipher: AesCfb8::new(key),
            inner,
        }
    }
}

impl<R: Read> Read for DecryptingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.cipher.decrypt(&mut buf[..n]);
        }
        Ok(n)
    }
}

/// Mojang session server 返回的 profile（`hasJoined` 响应）。
#[derive(Debug, Clone, Deserialize)]
pub struct GameProfile {
    /// 玩家 UUID（无连字符）。
    pub id: String,
    /// 玩家名。
    pub name: String,
    /// 属性（皮肤等）。
    #[serde(default)]
    pub properties: Vec<ProfileProperty>,
}

/// profile 属性（textures）。
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileProperty {
    /// 属性名。
    pub name: String,
    /// base64 值。
    pub value: String,
}

impl GameProfile {
    /// 解析 UUID。
    pub fn uuid(&self) -> Result<Uuid, CryptoError> {
        // Mojang 返回无连字符 UUID；带连字符时解析亦正确
        Uuid::parse_str(&self.id).map_err(|_| CryptoError::Parse)
    }
}

const MOJANG_SESSION_URL: &str =
    "https://sessionserver.mojang.com/session/minecraft/hasJoined?username={username}&serverId={server_hash}";

/// 调用 Mojang session server 验证玩家（正版模式）。
///
/// 客户端 `LoginKeyC2SPacket` 后，服务器已把玩家加入会话；此处查询
/// `hasJoined` 获取真实 profile。`server_hash` 来自 [`KeyStore::server_hash`]。
pub fn verify_session(
    username: &str,
    server_hash: &str,
    ip: Option<&str>,
) -> Result<GameProfile, CryptoError> {
    let url = MOJANG_SESSION_URL
        .replace("{username}", username)
        .replace("{server_hash}", server_hash);
    let url = match ip {
        Some(ip) => format!("{url}&ip={ip}"),
        None => url,
    };
    let mut response = ureq::get(&url).call().map_err(|err| match err {
        ureq::Error::StatusCode(code) => match code {
            204 => CryptoError::UnverifiedUsername,
            403 => CryptoError::Banned,
            other => CryptoError::AuthStatus(other),
        },
        _ => CryptoError::AuthRequest,
    })?;
    let profile: GameProfile = response
        .body_mut()
        .read_json()
        .map_err(|_| CryptoError::Parse)?;
    Ok(profile)
}

/// 登录配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineMode {
    /// 离线模式（默认；不校验 Mojang 会话）。
    Offline,
    /// 正版模式（RSA 加密 + session server 校验）。
    Online,
}

/// 解析正版模式开关（环境变量 `ICBM_ONLINE_MODE`：`true`/`1`/`online` 启用）。
#[must_use]
pub fn online_mode_from_env() -> OnlineMode {
    match std::env::var("ICBM_ONLINE_MODE").as_deref() {
        Ok("true" | "1" | "online") => OnlineMode::Online,
        _ => OnlineMode::Offline,
    }
}

/// 测试辅助：RSA 往返（客户端视角用公钥加密）。
#[cfg(test)]
fn encrypt_with_public_key(pub_der: &[u8], data: &[u8]) -> Vec<u8> {
    let pub_key = RsaPublicKey::from_public_key_der(pub_der).unwrap();
    pub_key
        .encrypt(&mut rand::thread_rng(), Pkcs1v15Encrypt, data)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::{KeyStore, auth_digest, encrypt_with_public_key};
    use std::io::{Read, Write};

    #[test]
    fn auth_digest_matches_mojang_format() {
        // 对照 master `auth_digest` 行为：SHA-1 摘要作为无符号 BigInt 转十六进制，
        // 无前导零（空摘要 → "0"）。
        let digest = auth_digest(&[]);
        assert_eq!(digest, "0");
        // 0x00 开头（补 0 字节）→ BigInt 移除前导零
        let digest = auth_digest(&[0x00, 0x12, 0x34]);
        assert_eq!(digest, "1234");
    }

    #[test]
    fn keystore_rsa_round_trip() {
        let store = KeyStore::new().unwrap();
        assert!(store.public_key_der().len() > 100);
        // 客户端用公钥加密，服务端私钥解密（PKCS1v15 往返）
        let secret = [0x13u8; 16];
        let cipher = encrypt_with_public_key(store.public_key_der(), &secret);
        let decrypted = store.decrypt(&cipher).unwrap();
        assert_eq!(decrypted, secret);
        // server hash 稳定
        assert_eq!(store.server_hash(&secret), store.server_hash(&secret));
    }

    #[test]
    fn aes_cfb8_stream_round_trip() {
        let key = [0x5Au8; 16];
        let plaintext = b"hello minecraft 1.21.1 encryption test".to_vec();
        // 客户端加密
        let mut encrypted = plaintext.clone();
        super::AesCfb8::new(&key).encrypt(&mut encrypted);
        // 服务端解密（同一 key/IV）
        let mut decrypted = encrypted.clone();
        super::AesCfb8::new(&key).decrypt(&mut decrypted);
        assert_eq!(decrypted, plaintext);
        // 写入器/读取器路径
        let mut writer = super::EncryptingWriter::new(Vec::new(), &key);
        writer.write_all(&plaintext).unwrap();
        writer.flush().unwrap();
        let bytes = writer.into_inner_writer();
        let mut reader = super::DecryptingReader::new(std::io::Cursor::new(bytes), &key);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, plaintext);
    }

    #[test]
    fn aes_cfb8_state_progresses_per_byte() {
        // CFB8 逐字节推进：同一明文不同位置的密文随 IV 演进而不同。
        let key = [0x5Au8; 16];
        let data = [0u8; 8];
        let mut cipher1 = super::AesCfb8::new(&key);
        let mut out1 = data;
        cipher1.encrypt(&mut out1);
        // 前两个字节应不同（IV 已推进）
        assert_ne!(out1[0], out1[1]);
        // 解密还原
        let mut back = out1;
        super::AesCfb8::new(&key).decrypt(&mut back);
        assert_eq!(back, data);
    }
}

impl<W: Write> EncryptingWriter<W> {
    /// 取出内部写入器（测试用）。
    #[cfg(test)]
    fn into_inner_writer(self) -> W {
        self.inner
    }
}
