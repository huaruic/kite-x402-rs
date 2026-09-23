//! Kite chain constants and USD -> token-unit pricing.
//!
//! Mirrors `kite.ts` and `kite.go` in the TypeScript and Go templates. Both of
//! those say you should not need to edit the file, and the same holds here: the
//! addresses, EIP-712 domains and the facilitator URL are what the Kite
//! facilitator validates signatures against.

/// One Kite network the wrapper can charge on.
///
/// Only the stablecoin the Kite facilitator settles for that network is listed.
/// The payer signs an EIP-3009 `transferWithAuthorization`, so the asset must
/// implement EIP-3009 and the EIP-712 domain (name/version) must match the
/// token contract exactly, or the facilitator rejects every signature.
#[derive(Debug, Clone, Copy)]
pub struct KiteChain {
    /// CAIP-2 network identifier.
    pub network: &'static str,
    /// Not used by the wrapper; carried so the struct matches `kite.ts` /
    /// `kite.go` and operators can read the RPC off the chain they picked.
    #[allow(dead_code)]
    pub rpc_url: &'static str,
    pub asset_address: &'static str,
    pub asset_symbol: &'static str,
    pub asset_decimals: u32,
    pub eip712_name: &'static str,
    pub eip712_version: &'static str,
}

/// Kite mainnet: Bridged USDC (USDC.e), 6 decimals.
pub const KITE_MAINNET: KiteChain = KiteChain {
    network: "eip155:2366",
    rpc_url: "https://rpc.gokite.ai",
    asset_address: "0x7aB6f3ed87C42eF0aDb67Ed95090f8bF5240149e",
    asset_symbol: "USDC.e",
    asset_decimals: 6,
    eip712_name: "Bridged USDC (Kite AI)",
    eip712_version: "2",
};

/// Kite testnet: pieUSD, 18 decimals. Kite Passport sandbox agents pay with this.
pub const KITE_TESTNET: KiteChain = KiteChain {
    network: "eip155:2368",
    rpc_url: "https://rpc-testnet.gokite.ai",
    asset_address: "0x38129cf4CE5E183eFF248F42A7D345Bb1B47621A",
    asset_symbol: "pieUSD",
    asset_decimals: 18,
    eip712_name: "pieUSD",
    eip712_version: "1",
};

/// Facilitator that verifies and settles on both Kite networks.
///
/// The `/v2` prefix stays: `/verify`, `/settle` and `/supported` are appended to
/// this base. Dropping it turns every call into a 404 that surfaces much later
/// as "settlement failed".
pub const FACILITATOR_URL: &str = "https://facilitator.pieverse.io/v2";

/// Resolves the `KITE_NETWORK` environment value.
pub fn chain_by_name(name: &str) -> Result<KiteChain, String> {
    match name.trim() {
        "" | "mainnet" => Ok(KITE_MAINNET),
        "testnet" => Ok(KITE_TESTNET),
        other => Err(format!(
            "unknown KITE_NETWORK {other:?} (want mainnet or testnet)"
        )),
    }
}

/// Converts a `$0.001`-style price into integer token units for `chain`.
///
/// The x402 SDKs ship a default asset table that does not include the Kite
/// stablecoins, which is why the TypeScript and Go templates register a custom
/// money parser. Rust has no x402 SDK at all, so this is the whole conversion:
/// exact decimal-string arithmetic, never floating point. `0.1 + 0.2` pricing
/// bugs settle real money on chain.
pub fn price_to_units(price: &str, chain: &KiteChain) -> Result<u128, String> {
    let text = price.trim();
    let text = text.strip_prefix('$').unwrap_or(text).trim();

    let (whole, frac) = match text.split_once('.') {
        Some((w, f)) => (w, f),
        None => (text, ""),
    };
    let digits_only = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits_only(whole) || (!frac.is_empty() && !digits_only(frac)) || text.ends_with('.') {
        return Err(format!("price must be a positive decimal, got {price:?}"));
    }

    let decimals = chain.asset_decimals as usize;
    if frac.len() > decimals {
        return Err(format!(
            "price {text} has more than {decimals} decimals ({})",
            chain.asset_symbol
        ));
    }

    let mut units_text = String::with_capacity(whole.len() + decimals);
    units_text.push_str(whole);
    units_text.push_str(frac);
    for _ in frac.len()..decimals {
        units_text.push('0');
    }

    let units: u128 = units_text
        .parse()
        .map_err(|_| format!("price {text} does not fit in 128 bits"))?;
    if units == 0 {
        return Err(format!(
            "price {text} is below one unit of {}",
            chain.asset_symbol
        ));
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_prices_convert_to_six_decimal_units() {
        let c = &KITE_MAINNET;
        assert_eq!(price_to_units("$0.001", c).unwrap(), 1_000);
        assert_eq!(price_to_units("0.001", c).unwrap(), 1_000);
        assert_eq!(price_to_units(" $1 ", c).unwrap(), 1_000_000);
        assert_eq!(price_to_units("$0.000001", c).unwrap(), 1);
        assert_eq!(price_to_units("$12.34", c).unwrap(), 12_340_000);
    }

    #[test]
    fn testnet_prices_convert_to_eighteen_decimal_units() {
        // Past u64::MAX, which is exactly why this is u128 and not u64.
        assert_eq!(
            price_to_units("$0.001", &KITE_TESTNET).unwrap(),
            1_000_000_000_000_000
        );
        assert_eq!(
            price_to_units("$25", &KITE_TESTNET).unwrap(),
            25_000_000_000_000_000_000
        );
    }

    #[test]
    fn rejects_prices_that_would_settle_wrong() {
        let c = &KITE_MAINNET;
        // More precision than the asset can represent: silently truncating here
        // would charge a different amount than the manifest advertises.
        assert!(price_to_units("$0.0000001", c).is_err());
        assert!(price_to_units("$0", c).is_err());
        assert!(price_to_units("$0.000000", c).is_err());
        assert!(price_to_units("$-1", c).is_err());
        assert!(price_to_units("$abc", c).is_err());
        assert!(price_to_units("$1.2.3", c).is_err());
        assert!(price_to_units("$1.", c).is_err());
        assert!(price_to_units("", c).is_err());
        assert!(price_to_units("$1e3", c).is_err());
    }

    #[test]
    fn network_names_match_the_other_templates() {
        assert_eq!(chain_by_name("").unwrap().network, "eip155:2366");
        assert_eq!(chain_by_name("mainnet").unwrap().network, "eip155:2366");
        assert_eq!(chain_by_name("testnet").unwrap().network, "eip155:2368");
        assert!(chain_by_name("devnet").is_err());
    }
}
