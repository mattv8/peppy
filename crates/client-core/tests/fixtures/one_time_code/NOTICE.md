# Third-party notice: one-time-code corpus

Entries in `positive.json` and `negative.json` whose `origin` is `parse-otp-message` are
derived from the test messages in `test/index.js` of
<https://github.com/transitive-bullshit/parse-otp-message> (fetched 2026-11-09). Only the
message text was used; the expected codes were re-checked against this detector's rules.

parse-otp-message is licensed under the MIT License (`"license": "MIT"` in its `package.json`):

```
MIT License

Copyright (c) Travis Fischer

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

Entries with other origins (`device`, `spec`, `hand`) were authored for this repository.
Device-derived codes are invented; no real codes are present.
