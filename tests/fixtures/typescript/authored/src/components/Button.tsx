// gold: (r) a props interface, a function component and a const arrow
// component.
export interface ButtonProps {
  label: string;
  onPress: () => void;
}

export function Button(props: ButtonProps) {
  return (
    <button className="btn" onClick={props.onPress}>
      {props.label}
    </button>
  );
}

export const Card = ({ children }: { children: string }) => (
  <div className="card">{children}</div>
);
