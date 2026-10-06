"""Read-only filtering/threshold contracts shared by optional research tools."""
UNFOLDED = 'NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid={0}.uuid AND h.hidden=1)'
VISIBLE = UNFOLDED + ' AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid={0}.uuid)'
STRONG_WD, STRONG_CCIP = 0.9, 0.95
MULTI_TAGS = ('multiple_girls', 'multiple_boys', 'multiple_others', '2girls', '3girls', '4girls', '5girls', '6+girls',
              '2boys', '3boys', '4boys', '5boys', '6+boys')
