//! List the permissions an admin role needs for one entity.
//!
//! ```bash
//! cargo run -p id_effect_admin --example 010_permissions
//! ```

use id_effect_admin::AdminPermission;

fn main() {
  match AdminPermission::all_for("document") {
    Ok(perms) => {
      for perm in perms {
        let kind = if perm.action().writes() {
          "write"
        } else {
          "read"
        };
        println!("{perm} ({kind})");
      }
    }
    Err(err) => eprintln!("invalid entity: {err}"),
  }
}
