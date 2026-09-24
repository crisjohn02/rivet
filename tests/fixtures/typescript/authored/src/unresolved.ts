import { twice } from "./barrel";
import { Status } from "./barrel";
import { format as aliasFormat } from "@/util";
import { debounce } from "lodash";
import type { Survey } from "./models";
import { pick } from "./pick";

// gold: (x) import-bound uses below stay unresolved in v0.1, except (b) the
// import type; (y) "./pick" is two modules.
export function unresolvedUses(survey: Survey): number {
  const legacy = require("./util");
  legacy.double(1);
  aliasFormat(2);
  debounce(twice);
  pick(survey.id);
  return twice(Status.Draft);
}
