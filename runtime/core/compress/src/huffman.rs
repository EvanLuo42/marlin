//! Prefix-code construction for the encoders (zstd literals, Deflate).

use alloc::vec::Vec;

/// Optimal prefix-code lengths no longer than `max_len` for `freqs` (package-merge). Unused
/// symbols get 0. With a single used symbol it gets length 1, so the code is incomplete; the
/// callers that need a complete code handle that case themselves. Requires
/// `2^max_len >= used symbols`.
pub(crate) fn code_lengths(freqs: &[u32], max_len: u32, lens: &mut [u8]) {
    lens[..freqs.len()].fill(0);
    let mut leaves: Vec<(u64, u32)> = freqs
        .iter()
        .enumerate()
        .filter(|(_, f)| **f > 0)
        .map(|(s, f)| (*f as u64, s as u32))
        .collect();
    match leaves.len() {
        0 => return,
        1 => {
            lens[leaves[0].1 as usize] = 1;
            return;
        }
        _ => {}
    }
    debug_assert!(leaves.len() <= 1 << max_len);
    leaves.sort_unstable();

    // Nodes: leaves first (`child == NO_CHILD`, `sym` set), then packages of two nodes.
    const NO_CHILD: u32 = u32::MAX;
    struct Node {
        weight: u64,
        a: u32,
        b: u32,
    }
    let mut nodes: Vec<Node> = leaves
        .iter()
        .map(|&(w, s)| Node {
            weight: w,
            a: NO_CHILD,
            b: s,
        })
        .collect();
    let n = leaves.len();
    let mut list: Vec<u32> = (0..n as u32).collect();
    for _ in 1..max_len {
        let mut merged = Vec::with_capacity(n + list.len() / 2);
        let mut li = 0;
        let mut pi = 0;
        let packages = list.len() / 2;
        while li < n || pi < packages {
            let take_leaf = if li == n {
                false
            } else if pi == packages {
                true
            } else {
                let pw =
                    nodes[list[2 * pi] as usize].weight + nodes[list[2 * pi + 1] as usize].weight;
                nodes[li].weight <= pw
            };
            if take_leaf {
                merged.push(li as u32);
                li += 1;
            } else {
                let (a, b) = (list[2 * pi], list[2 * pi + 1]);
                let weight = nodes[a as usize].weight + nodes[b as usize].weight;
                nodes.push(Node { weight, a, b });
                merged.push(nodes.len() as u32 - 1);
                pi += 1;
            }
        }
        list = merged;
    }
    // Each time a leaf appears among the first 2n - 2 items (counting inside packages), its
    // code gets one bit longer.
    let mut stack: Vec<u32> = list[..2 * n - 2].to_vec();
    while let Some(i) = stack.pop() {
        let node = &nodes[i as usize];
        if node.a == NO_CHILD {
            lens[node.b as usize] += 1;
        } else {
            stack.push(node.a);
            stack.push(node.b);
        }
    }
}

/// Canonical codes for `lens` (RFC 1951 3.2.2), returned bit-reversed: Deflate packs codes
/// MSB-first into an LSB-first stream, so the reversed code is what gets added to the stream.
pub(crate) fn deflate_codes(lens: &[u8], codes: &mut [u16]) {
    let mut count = [0u16; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    for (s, &l) in lens.iter().enumerate() {
        if l != 0 {
            let c = next[l as usize];
            next[l as usize] += 1;
            codes[s] = c.reverse_bits() >> (16 - l);
        } else {
            codes[s] = 0;
        }
    }
}
