// gold: (aa) directory-only specifiers (T44a): "." and "../pick/" name only
// src/pick/index.ts; "../pick" is still two modules (y).
import { pick as fromDot } from ".";
import { pick as fromDir } from "../pick/";
import { pick as fromFile } from "../pick";

fromDot(1);
fromDir(2);
fromFile(3);
