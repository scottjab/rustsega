//! The two rules a `.srm` has to get right, checked without a browser (#398).

use web::srm;

#[test]
fn a_srm_is_named_for_the_sha1_that_keys_it() {
    assert_eq!(
        srm::file_name("2b8fdb67ec2b6c6c0e5b6b8e3ba6b8d6c4d3f2a1"),
        "2b8fdb67ec2b6c6c0e5b6b8e3ba6b8d6c4d3f2a1.srm"
    );
    assert!(srm::file_name("abc").ends_with(srm::SRAM_SUFFIX));
}

#[test]
fn an_import_of_exactly_the_carts_size_is_accepted() {
    assert_eq!(srm::check_import(32 * 1024, 32 * 1024), Ok(()));
}

/// The wrong-cart case, named in the ticket: a save from a different cartridge is
/// a different size, and it must be refused with both sizes in what the player is
/// told. `genesis` refuses such a blob whole and silently
/// (`genesis::cart::sram::Sram::load`); this is the layer that has to say so.
#[test]
fn a_save_of_another_size_is_refused_and_says_both_sizes() {
    let error = srm::check_import(8 * 1024, 32 * 1024).expect_err("8 KiB is not 32 KiB");
    assert_eq!(
        error,
        srm::ImportError::WrongSize {
            got: 8 * 1024,
            want: 32 * 1024,
        }
    );
    let said = error.to_string();
    assert!(
        said.contains("8192") && said.contains("32768"),
        "the toast has to name both sizes, and said {said:?}"
    );
}

#[test]
fn an_empty_file_is_refused_too() {
    // A zero-byte pick is the most likely "wrong file" of all, and it is refused
    // rather than becoming the empty save that blanks a real one.
    assert!(srm::check_import(0, 32 * 1024).is_err());
}
