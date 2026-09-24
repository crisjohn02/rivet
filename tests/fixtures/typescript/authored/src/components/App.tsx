import { Button, Card } from "./Button";
import { double } from "../util";

// gold: (s) capitalized components are uses; intrinsic elements, JSX text and
// string attribute values are not.
export function App() {
  const count = double(2);
  return (
    <>
      <Button label="Launch" onPress={() => double(count)} />
      <Card>Button launch text</Card>
      <span title="Button">{count}</span>
    </>
  );
}

// gold: (q) an anonymous default-exported function component.
export default function () {
  return <App />;
}
