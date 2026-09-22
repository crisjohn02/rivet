<?php

declare(strict_types=1);

namespace App\Enums;

enum Suit
{
    case Hearts;
    case Spades;
}

enum Status: string
{
    case Active = 'active';
    case Closed = 'closed';
}
