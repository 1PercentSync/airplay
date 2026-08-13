//! CSPRNG without extra crates: BCrypt on Windows, `/dev/urandom` on Unix.
//! [evidence: research/05 — no `rand` / `getrandom` on the whitelist]

use airplay_core::{Error, Result};

pub fn fill_random(buf: &mut [u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?
            .read_exact(buf)
            .map_err(Error::from)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        unsafe {
            BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
                .ok()
                .map_err(|e| Error::Srp(format!("BCryptGenRandom: {e}")))?;
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = buf;
        Err(Error::Srp("no CSPRNG on this platform".into()))
    }
}

pub fn random_u64() -> Result<u64> {
    let mut b = [0u8; 8];
    fill_random(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub fn random_u32() -> Result<u32> {
    Ok(random_u64()? as u32)
}
