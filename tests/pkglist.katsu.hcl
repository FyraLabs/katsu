pkg_list "test" "a" {
    all = [
        "a",
        "b",
        "c"
    ]

    exclude = {
        all = [
            "b"
        ]
    }
}   