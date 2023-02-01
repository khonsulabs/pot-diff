use std::collections::BTreeMap;

use pot_diff::{Diff, Diffable};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Eq, PartialEq, Debug)]
pub struct UserProfile {
    name: String,
    email: String,
    age: u8,
    attributes: BTreeMap<String, String>,
}

fn main() {
    // First, we start with a Serde-compatible data structure. We serialize it
    // and send it to the client.
    let mut server_user = Diffable::new(UserProfile {
        name: String::from("ecton"),
        email: String::from("support@khonsulabs.com"),
        age: 99,
        attributes: [
            (String::from("color"), String::from("red")),
            (String::from("language"), String::from("english")),
        ]
        .into_iter()
        .collect(),
    });
    let initial_payload = pot::to_vec(&*server_user).unwrap();

    // The client is able to deserialize the user profile normally.
    let client_user: UserProfile = pot::from_slice(&initial_payload).unwrap();

    // Now, the server updates some information and wants to send it to the
    // client.
    server_user
        .attributes
        .insert(String::from("os"), String::from("manjaro"));
    server_user
        .attributes
        .insert(String::from("color"), String::from("green"));
    let diff = server_user.diff().expect("changes were made");
    let diff_payload = diff.serialize();

    // Now the client can observe the updates by deserializing the payload and
    // applying it.
    let client_diff = Diff::deserialize(&diff_payload).unwrap();
    let updated_user = client_diff.apply(&client_user).unwrap();
    assert_eq!(updated_user, *server_user);

    println!("Full user payload: {} bytes", initial_payload.len());
    println!("Diff payload: {} bytes", diff_payload.len());
    println!("Diff contents: {diff}");
}

#[test]
fn runs() {
    main();
}
