Feature: Reading an interval from a string

  A string cast to an interval is read by Spark's `stringToInterval` for the
  calendar type and by `castStringToDTInterval` / `castStringToYMInterval` for
  the qualified ones. These are the shapes those accept, and the ones they turn
  down; the scenarios run against Spark too, so a divergence shows up as a
  failure rather than as a number nobody looks at.

  Rule: ANSI shapes

    Scenario Outline: Day-time: <case>
      When query
        """
        SELECT CAST('<value>' AS INTERVAL DAY TO SECOND) AS result
        """
      Then query result
        | result   |
        | <result> |

      Examples:
        | case                       | value               | result                               |
        | day and time               | 1 02:03:04          | INTERVAL '1 02:03:04' DAY TO SECOND  |
        | negative                   | -1 02:03:04         | INTERVAL '-1 02:03:04' DAY TO SECOND |
        | a leading plus is dropped  | +1 02:03:04         | INTERVAL '1 02:03:04' DAY TO SECOND  |
        | single digit components    | 1 2:3:4             | INTERVAL '1 02:03:04' DAY TO SECOND  |
        | leading zeros              | 01 02:03:04         | INTERVAL '1 02:03:04' DAY TO SECOND  |
        | fraction of a second       | 1 02:03:04.5        | INTERVAL '1 02:03:04.5' DAY TO SECOND |
        | fraction truncates to six  | 1 02:03:04.12345678 | INTERVAL '1 02:03:04.123456' DAY TO SECOND |
        | the last valid time        | 1 23:59:59          | INTERVAL '1 23:59:59' DAY TO SECOND  |

    Scenario Outline: Day-time refused: <case>
      When query
        """
        SELECT CAST('<value>' AS INTERVAL DAY TO SECOND) AS result
        """
      Then query error (?i)invalid.*interval

      Examples:
        | case                          | value                 |
        | two spaces before the time    | 1  02:03:04           |
        | a missing component           | 1 02:03               |
        | a sign inside the value       | 1 -02:03:04           |
        | a dot with no digits          | 1 02:03:04.           |
        | ten fraction digits           | 1 02:03:04.1234567890 |

    Scenario Outline: A field outside its range is refused: <case>
      When query
        """
        SELECT CAST('<value>' AS INTERVAL DAY TO SECOND) AS result
        """
      # Both engines refuse, and word it differently: Spark names the field and
      # its range, Sail names the string it could not read.
      Then query error (?i)(outside range|invalid.*interval)

      Examples:
        | case                   | value      |
        | hour past the day      | 1 25:00:00 |
        | minute past the hour   | 1 02:60:00 |
        | second past the minute | 1 02:03:60 |

    Scenario Outline: Year-month: <case>
      When query
        """
        SELECT CAST('<value>' AS INTERVAL YEAR TO MONTH) AS result
        """
      Then query result
        | result   |
        | <result> |

      Examples:
        | case          | value  | result                        |
        | years months  | 1-2    | INTERVAL '1-2' YEAR TO MONTH  |
        | negative      | -1-2   | INTERVAL '-1-2' YEAR TO MONTH |
        | eleven months | 1-11   | INTERVAL '1-11' YEAR TO MONTH |

    Scenario: months past a year are refused
      When query
        """
        SELECT CAST('0-13' AS INTERVAL YEAR TO MONTH) AS result
        """
      Then query error (?i)(outside range|invalid.*interval)

  Rule: The rendering Spark prints

    Scenario Outline: Round trip: <case>
      When query
        """
        SELECT CAST(CAST(<lit> AS STRING) AS <type>) AS result
        """
      Then query result
        | result   |
        | <result> |

      Examples:
        | case          | lit                          | type                   | result                       |
        | year to month | INTERVAL '1-3' YEAR TO MONTH | INTERVAL YEAR TO MONTH | INTERVAL '1-3' YEAR TO MONTH |

    Scenario: a sign before the value negates it
      When query
        """
        SELECT CAST('INTERVAL -\'-1-2\' YEAR TO MONTH' AS INTERVAL YEAR TO MONTH) AS result
        """
      Then query result
        | result                       |
        | INTERVAL '1-2' YEAR TO MONTH |

  Rule: Multi-unit shapes

    Scenario Outline: Calendar: <case>
      When query
        """
        SELECT CAST('<value>' AS INTERVAL) AS result
        """
      Then query result
        | result   |
        | <result> |

      Examples:
        | case                        | value           | result             |
        | one term                    | 5 seconds       | 5 seconds          |
        | two terms                   | 1 day 2 hour    | 1 days 2 hours     |
        | a sign between terms        | 1 day - 2 hour  | 1 days -2 hours    |
        | a plus between terms        | 1 day + 2 hour  | 1 days 2 hours     |
        | a sign of its own           | - 1 day         | -1 days            |
        | the keyword is allowed      | interval 1 day  | 1 days             |
        | a dot with no digits        | 1. seconds      | 1 seconds          |
        | no integer part             | .5 seconds      | 0.5 seconds        |
        | the smallest month count    | -2147483648 month | -178956970 years -8 months |

    Scenario Outline: Calendar yields NULL: <case>
      When query
        """
        SELECT CAST('<value>' AS INTERVAL) AS result
        """
      Then query result
        | result |
        | NULL   |

      Examples:
        | case                       | value          |
        | a doubled sign             | - -5 minutes   |
        | a sign with no space       | 1 day+2 hour   |
        | a value with no space      | 1day           |
        | an abbreviated unit        | 5 secs         |
        | a fraction on a day        | 1.5 days       |
        | nothing to read            | garbage        |
        | an ANSI shape              | 1 02:03:04     |
        | an ANSI shape out of range | 1 25:00:00     |

  Rule: TRY_CAST yields NULL where CAST errors

    Scenario Outline: TRY_CAST: <case>
      When query
        """
        SELECT TRY_CAST('<value>' AS <type>) AS result
        """
      Then query result
        | result |
        | NULL   |

      Examples:
        | case                  | value      | type                   |
        | unreadable day-time   | garbage    | INTERVAL DAY TO SECOND |
        | out of range          | 1 25:00:00 | INTERVAL DAY TO SECOND |
        | unreadable year-month | garbage    | INTERVAL YEAR TO MONTH |

  Rule: The qualified SQL literal reads the same ranges

    Scenario Outline: Literal refused: <case>
      When query
        """
        SELECT INTERVAL '<value>' <qualifier> AS result
        """
      Then query error (?i)(outside range|invalid.*interval)

      Examples:
        | case                 | value                 | qualifier     |
        | hour past the day    | 1 25:00:00            | DAY TO SECOND |
        | minute past the hour | 1 02:60:00            | DAY TO SECOND |
        | second past a minute | 1 02:03:60            | DAY TO SECOND |
        | a three-digit field  | 1 002:03:04           | DAY TO SECOND |
        | ten fraction digits  | 1 02:03:04.1234567890 | DAY TO SECOND |
        | months past a year   | 0-13                  | YEAR TO MONTH |
