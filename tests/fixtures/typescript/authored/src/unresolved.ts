import { twice } from "./barrel";
import { Status } from "./barrel";
import { format as aliasFormat } from "@/util";
import { debounce } from "lodash";
import type { Survey } from "./models";
import { pick } from "./pick";

// gold: (x) every import-bound use below stays unresolved in v0.1; (y) "./pick"
// names both pick.ts and pick/index.ts.
export function unresolvedUses(survey: Survey): number {
  const legacy = require("./util");
  legacy.double(1);
  aliasFormat(2);
  debounce(twice);
  pick(survey.id);
  return twice(Status.Draft);
}
