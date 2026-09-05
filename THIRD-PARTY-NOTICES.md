# Third-party notices

onix is licensed under the MIT License (see [LICENSE](LICENSE)). Portions of
onix reimplement algorithms from the projects below; their notices and
licenses are reproduced here as required.

---

## CPython — `difflib`

`crates/onix-core/src/lcs.rs` and `crates/onix-core/src/unified_diff.rs`
reimplement, from the reference source, algorithms from the Python standard
library's `difflib` module: `SequenceMatcher` (`find_longest_match`,
`get_matching_blocks`, `get_opcodes`, `get_grouped_opcodes`, and the autojunk
heuristic in `__chain_b`), `unified_diff`, and `_format_range_unified`. The
source was read from CPython 3.14.6.

CPython is copyright © 2001-2025 Python Software Foundation; all rights
reserved, and is used under the PSF License Agreement, reproduced below.

```
PYTHON SOFTWARE FOUNDATION LICENSE VERSION 2
--------------------------------------------

1. This LICENSE AGREEMENT is between the Python Software Foundation ("PSF"), and
   the Individual or Organization ("Licensee") accessing and otherwise using this
   software ("Python") in source or binary form and its associated documentation.

2. Subject to the terms and conditions of this License Agreement, PSF hereby
   grants Licensee a nonexclusive, royalty-free, world-wide license to reproduce,
   analyze, test, perform and/or display publicly, prepare derivative works,
   distribute, and otherwise use Python alone or in any derivative version,
   provided, however, that PSF's License Agreement and PSF's notice of copyright,
   i.e., "Copyright (c) 2001 Python Software Foundation; All Rights Reserved"
   are retained in Python alone or in any derivative version prepared by Licensee.

3. In the event Licensee prepares a derivative work that is based on or
   incorporates Python or any part thereof, and wants to make the derivative work
   available to others as provided herein, then Licensee hereby agrees to include
   in any such work a brief summary of the changes made to Python.

4. PSF is making Python available to Licensee on an "AS IS" basis. PSF MAKES NO
   REPRESENTATIONS OR WARRANTIES, EXPRESS OR IMPLIED. BY WAY OF EXAMPLE, BUT NOT
   LIMITATION, PSF MAKES NO AND DISCLAIMS ANY REPRESENTATION OR WARRANTY OF
   MERCHANTABILITY OR FITNESS FOR ANY PARTICULAR PURPOSE OR THAT THE USE OF PYTHON
   WILL NOT INFRINGE ANY THIRD PARTY RIGHTS.

5. PSF SHALL NOT BE LIABLE TO LICENSEE OR ANY OTHER USERS OF PYTHON FOR ANY
   INCIDENTAL, SPECIAL, OR CONSEQUENTIAL DAMAGES OR LOSS AS A RESULT OF MODIFYING,
   DISTRIBUTING, OR OTHERWISE USING PYTHON, OR ANY DERIVATIVE THEREOF, EVEN IF
   ADVISED OF THE POSSIBILITY THEREOF.

6. This License Agreement will automatically terminate upon a material breach of
   its terms and conditions.

7. Nothing in this License Agreement shall be deemed to create any relationship
   of agency, partnership, or joint venture between PSF and Licensee. This License
   Agreement does not grant permission to use PSF trademarks or trade name in a
   trademark sense to endorse or promote products or services of Licensee, or any
   third party.

8. By copying, installing or otherwise using Python, Licensee agrees to be bound
   by the terms and conditions of this License Agreement.
```

Changes made (per clause 3): the algorithms are reimplemented in Rust over
onix's own scalar value model rather than Python objects; junk handling is
omitted (`isjunk` is always `None`); and the autojunk heuristic is applied on
the multi-line string diff path but disabled on the ordered-list comparison
path. See the module docs in `lcs.rs` and `unified_diff.rs` for details.

---

## DeepDiff

The `ignore_order` engine (`crates/onix-core/src/ignore_order/`), the
ordered-list `difflib`-selection and mutual-add-remove-merge behavior
(`crates/onix-core/src/diff/`, `crates/onix-core/src/report.rs`), and the
`_diff_str` convenience diff reimplemented in
`crates/onix-core/src/unified_diff.rs` reproduce the observable behavior of
`DeepDiff` (its `DeepHash`, `_diff_iterable_with_deephash`,
`_diff_ordered_iterable_by_difflib`, and `_diff_str`), differentially tested
against `deepdiff==9.1.0`.

DeepDiff is copyright © Sep Dehpour (Seperman) and contributors, and is used
under the MIT License:

```
The MIT License (MIT)

Copyright (c) 2014 - 2026 Sep Dehpour (Seperman) and contributors
getqluster.com
zepworks.com

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
