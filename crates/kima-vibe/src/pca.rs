//! The map's start: the 3 main axes of the library's centred matrix by subspace iteration, scaled like umap-learn's
//! `noisy_scale_coords` (port of upstream `mapLayout.ts` `symmetricEigen`, `principalScores`, `principalStart`).

// Index loops mirror the TypeScript line for line: the port is checked bit for bit against it.
#![allow(clippy::needless_range_loop)]

use crate::prng::{SeededRandom, gaussian};

/// Symmetric eigen-decomposition by Jacobi rotations (100 sweeps, off-diagonal sum below 1e-30); the columns of
/// `vectors` match `values`, largest first (ties keep their order).
pub(crate) fn symmetric_eigen(m: &[Vec<f64>]) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = m.len();
    let mut a: Vec<Vec<f64>> = m.to_vec();
    let mut v: Vec<Vec<f64>> = (0..n)
        .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    for _ in 0..100 {
        let mut off = 0.0;
        for p in 0..n {
            for q in p + 1..n {
                off += a[p][q] * a[p][q];
            }
        }
        if off < 1e-30 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                // Math.sign(theta || 1): zero and NaN count as 1.
                let sign = if theta == 0.0 || theta.is_nan() || theta > 0.0 {
                    1.0
                } else {
                    -1.0
                };
                let t = sign / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for row in a.iter_mut() {
                    let (arp, arq) = (row[p], row[q]);
                    row[p] = c * arp - s * arq;
                    row[q] = s * arp + c * arq;
                }
                for r in 0..n {
                    let (apr, aqr) = (a[p][r], a[q][r]);
                    a[p][r] = c * apr - s * aqr;
                    a[q][r] = s * apr + c * aqr;
                }
                for row in v.iter_mut() {
                    let (vrp, vrq) = (row[p], row[q]);
                    row[p] = c * vrp - s * vrq;
                    row[q] = s * vrp + c * vrq;
                }
            }
        }
    }
    let mut order: Vec<(f64, usize)> = (0..n).map(|i| (a[i][i], i)).collect();
    // A stable sort, descending by eigenvalue, as `Array.prototype.sort` with `y - x`.
    order.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
    let values = order.iter().map(|(val, _)| *val).collect();
    let vectors = v
        .iter()
        .map(|row| order.iter().map(|(_, i)| row[*i]).collect())
        .collect();
    (values, vectors)
}

/// The 3 main axes of the centred `n x d` matrix behind `multiply` (X.v) and `multiply_t` (Xt.u), as scores `n x 3`
/// (track-major): subspace iteration on XtX with 10 extra directions from a seeded start, then the 3 largest Ritz
/// pairs; each axis's sign set so its largest entry is positive, as scikit-learn's `svd_flip` does. A matrix with
/// fewer than 3 columns leaves the missing axes at zero.
pub fn principal_scores(
    n: usize,
    d: usize,
    multiply: &dyn Fn(&[f64]) -> Vec<f64>,
    multiply_t: &dyn Fn(&[f64]) -> Vec<f64>,
    seed: u32,
) -> Vec<f64> {
    let mut rng = SeededRandom::new(seed);
    let width = d.min(13);
    let orthonormalise = |vs: Vec<Vec<f64>>| -> Vec<Vec<f64>> {
        let mut out: Vec<Vec<f64>> = Vec::with_capacity(vs.len());
        for v in vs {
            let mut w = v;
            for _ in 0..2 {
                for u in &out {
                    let mut p = 0.0;
                    for i in 0..d {
                        p += w[i] * u[i];
                    }
                    for i in 0..d {
                        w[i] -= p * u[i];
                    }
                }
            }
            let mut norm = 0.0;
            for x in &w {
                norm += x * x;
            }
            norm = norm.sqrt();
            if norm == 0.0 || norm.is_nan() {
                norm = 1.0;
            }
            for x in w.iter_mut() {
                *x /= norm;
            }
            out.push(w);
        }
        out
    };
    let ritz = |basis: &[Vec<f64>]| {
        let projected: Vec<Vec<f64>> = basis.iter().map(|v| multiply(v)).collect();
        let w = basis.len();
        let mut small = vec![vec![0.0; w]; w];
        // Symmetric: each product sums in the same order either way round.
        for r in 0..w {
            for c in r..w {
                let mut p = 0.0;
                for i in 0..n {
                    p += projected[r][i] * projected[c][i];
                }
                small[r][c] = p;
                small[c][r] = p;
            }
        }
        symmetric_eigen(&small)
    };
    let start: Vec<Vec<f64>> = (0..width)
        .map(|_| (0..d).map(|_| gaussian(&mut rng)).collect())
        .collect();
    let mut basis = orthonormalise(start);
    let mut previous = [0.0_f64; 3];
    for _ in 0..200 {
        basis = orthonormalise(basis.iter().map(|v| multiply_t(&multiply(v))).collect());
        let (values, _) = ritz(&basis);
        let top = &values[..values.len().min(3)];
        let settled = top
            .iter()
            .enumerate()
            .all(|(i, v)| (v - previous[i]).abs() <= 1e-12 * v.abs());
        for (i, v) in top.iter().enumerate() {
            previous[i] = *v;
        }
        if settled {
            break;
        }
    }
    let (_, vectors) = ritz(&basis);
    let axes: Vec<Vec<f64>> = (0..3)
        .map(|c| {
            let mut axis = vec![0.0; d];
            if c < basis.len() {
                for (r, b) in basis.iter().enumerate() {
                    for i in 0..d {
                        axis[i] += vectors[r][c] * b[i];
                    }
                }
                let mut big = 0.0_f64;
                for x in &axis {
                    if x.abs() > big.abs() {
                        big = *x;
                    }
                }
                if big < 0.0 {
                    for x in axis.iter_mut() {
                        *x = -*x;
                    }
                }
            }
            axis
        })
        .collect();
    let scores: Vec<Vec<f64>> = axes.iter().map(|a| multiply(a)).collect();
    let mut out = vec![0.0; n * 3];
    for i in 0..n {
        for c in 0..3 {
            out[i * 3 + c] = scores[c][i];
        }
    }
    out
}

/// umap-learn's start from the main axes (`noisy_scale_coords`): largest coordinate 10, then noise of 1e-4.
pub fn principal_start(
    n: usize,
    d: usize,
    multiply: &dyn Fn(&[f64]) -> Vec<f64>,
    multiply_t: &dyn Fn(&[f64]) -> Vec<f64>,
    seed: u32,
) -> Vec<f64> {
    let scores = principal_scores(n, d, multiply, multiply_t, seed);
    let mut largest = 0.0_f64;
    for x in &scores {
        largest = largest.max(x.abs());
    }
    let scale = 10.0 / if largest == 0.0 { 1.0 } else { largest };
    let mut rng = SeededRandom::new(seed.wrapping_add(1));
    scores
        .iter()
        .map(|x| x * scale + 1e-4 * gaussian(&mut rng))
        .collect()
}
