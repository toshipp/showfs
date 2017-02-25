[![wercker status](https://app.wercker.com/status/1e436817198aa6ce7b3f47b3cd1ca9c4/s/master "wercker status")](https://app.wercker.com/project/byKey/1e436817198aa6ce7b3f47b3cd1ca9c4)

showfs - mount an archive as a directory.
-----------------------------------------

* build

    ```
    cargo build
    ```

* test

    ```
    ./tool/make_assets
    cargo test
    ```

* usage

    ```
    showfs $ARCHIVE $DIR
    showfs $DIR_CONTAINING_ARCHIVE $DIR
    ```
