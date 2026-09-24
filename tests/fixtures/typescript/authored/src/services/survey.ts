import { Status, Survey } from "../models";

// gold: (k) an abstract class with an abstract and a protected method.
export abstract class BaseService {
  abstract describe(): string;

  protected log(message: string): void {
    console.log(message);
  }
}

// gold: (a) SurveyService.launch; ReportService.launch shares its name.
export class SurveyService extends BaseService {
  // gold: (k) a static field.
  static instances = 0;

  // gold: (k) an annotated field and an ES private field.
  private current: Survey | null = null;
  #attempts = 0;

  // gold: (k) a constructor with parameter properties.
  constructor(private readonly prefix: string, public status: Status) {
    super();
    SurveyService.instances += 1;
  }

  // gold: (k) a static method; its new expression is a type use.
  static create(): SurveyService {
    return new SurveyService("survey", Status.Draft);
  }

  describe(): string {
    return `${this.prefix}: ${this.status}`;
  }

  // gold: (h) the string below is not a use; (i) this.log is inherited.
  launch(): void {
    this.#attempts += 1;
    this.log("launch");
  }

  // gold: (d) this.launch() is scoped through this.
  relaunch(): void {
    this.launch();
  }

  // gold: (u) a getter and setter pair sharing one name.
  get label(): string {
    return this.prefix;
  }

  set label(value: string) {
    this.log(value);
  }
}
