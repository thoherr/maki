# simple digital asset manager

## mandatory requirements

- suitable for all kinds of digital assets, especially images and videos
- all formats of RAW files must be supported (e.g. NEF, RAF etc.)
- multiple variants of the same asset must be grouped and/or navigatable (e.g. RAW / JPEG, different processing etc.)
- media files can be stored on one or more offline devices (we are talking about terrabytes)
- duplicates should be stored only once
- original files (and maybe also variants?) can move (e.g. on different storage device) transparently
- processing instructions / recipies etc. should be managed as well. This should include software like CaptureOne, Photoshop etc.

## basic technical ideas

- an original file (most of the time this is the RAW file) is never changed, so we can make it content adressable (e.g. SHA)
- metadata stored in sidecar files (how can we link media and sidecar)?
- navigation / retrieval independent of location of media files 
- all should be text based
- should we use git as backend (but probably only the storage part)?


