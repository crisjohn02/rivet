import { SurveyService } from "./services/survey";
import { Status } from "./models";
import { launchAll as runAll, double } from "./util";
import makeLabel from "./util";
import * as util from "./util";

// gold: (a) a second class with a same-name launch method.
export class ReportService {
  // gold: (f) a field annotation types this.service.
  private service: SurveyService;

  constructor(service: SurveyService) {
    this.service = service;
  }

  launch(): void {
    this.service.launch();
  }

  // gold: (e) scoped through a preceding new.
  runNew(): void {
    const created = new SurveyService("report", Status.Active);
    created.launch();
  }

  // gold: (f) scoped through a parameter annotation, with optional chaining.
  runTyped(svc: SurveyService): void {
    svc.launch();
    svc?.launch();
  }

  // gold: (f) scoped through a variable annotation.
  runVariable(): void {
    const annotated: SurveyService = SurveyService.create();
    annotated.launch();
  }

  // gold: (g) an unannotated receiver stays name_match.
  runUnknown(items: ReportService[]): void {
    const x = items[0];
    x.launch();
  }
}

// gold: (b) an aliased import and a renamed default import; (c) top-level uses.
runAll(2);
double(3);
makeLabel();

// gold: (o) a namespace import used as ns.member.
util.format(4);

// gold: (h) template literal text is not code; its interpolations are.
export const summary = `café launch ${double(5)} via ${util.format(6)}`;

// gold: (p) a local of the same name as an import shadows it.
export function shadowed(): number {
  const double = (n: number): number => n + n;
  return double(7);
}
