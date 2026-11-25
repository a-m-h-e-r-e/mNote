fn main() {
// <- @keyword
//  ^ @function
    let name = "mNote";
//  ^ @keyword
//             ^ @string
    // a comment
//  ^ @comment
    println!("{}", 42);
//  ^ @function.macro
}
