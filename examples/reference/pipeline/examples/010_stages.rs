//! Print the Pipeline reference app's stages in order.
//!
//! ```bash
//! cargo run -p pipeline --example 010_stages
//! ```

use pipeline::stages;

fn main() {
  match stages().block_on().into_result() {
    Ok(all) => {
      for (i, stage) in all.iter().enumerate() {
        println!("{}. {stage}", i + 1);
      }
    }
    Err(cause) => eprintln!("stages failed: {cause:?}"),
  }
}
